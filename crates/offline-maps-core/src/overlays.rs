//! Overlays for a region: flood hazard zones (FEMA National Flood Hazard
//! Layer) and public lands (USGS PAD-US) as vector tiles in one MBTiles file,
//! built by `tools/layers/build_overlays.py` and published with the GitHub
//! releases. Both sources are public domain.
//!
//! ```text
//! <root>/<region>/overlays/active.json
//! <root>/<region>/overlays/versions/<v>/overlays.mbtiles
//! <root>/<region>/overlays/versions/<v>/record.json
//! <root>/<region>/overlays/staging/package.part      download in progress
//! ```
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::download::{
    Progress, Transport, UpdateError, UpdateOptions, check_size, download_to_staging, network,
};
use crate::mbtiles::{Bounds, PackageError, TilePackage};
use crate::package::{Region, SourceInfo, sha256_file};
use crate::store::{PackageStore, RECORD_FILE, is_version_name, write_atomic};

/// Where the overlay files are published: `<base>/<region>-overlays.mbtiles`.
pub const OVERLAYS_BASE: &str =
    "https://github.com/ianrastall/offline-us-maps/releases/download/overlays";
pub const OVERLAYS_FILE: &str = "overlays.mbtiles";
/// Layers every overlay file must have.
pub const OVERLAY_LAYERS: [&str; 3] = ["flood_zones", "public_lands", "public_land_labels"];
pub const OVERLAYS_ATTRIBUTION: &str =
    "Flood zones: FEMA National Flood Hazard Layer. Public lands: USGS PAD-US. Both public domain.";
const RECORD_SCHEMA: u32 = 1;

/// The store ID under which a region's overlays are kept (`us-il/overlays`).
pub fn overlays_id(region_id: &str) -> String {
    format!("{region_id}/overlays")
}

/// The published file for a region under `base`.
pub fn overlays_url(base: &str, region_id: &str) -> String {
    format!(
        "{}/{region_id}-overlays.mbtiles",
        base.trim_end_matches('/')
    )
}

/// What the build recorded about its sources (metadata keys
/// `offline-us-maps:*` in the file).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OverlaySources {
    /// FEMA's product name, e.g. `NFHL_17_20261027`.
    pub flood_product: String,
    /// Effective date of the flood data as FEMA states it.
    pub flood_effective: String,
    pub public_lands: String,
    pub built_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayRecord {
    pub schema: u32,
    pub region_id: String,
    pub source: SourceInfo,
    pub byte_length: u64,
    pub sha256: String,
    pub downloaded_at: String,
    pub bounds: Bounds,
    pub sources: OverlaySources,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveOverlays {
    pub version: String,
    pub path: PathBuf,
    pub record: OverlayRecord,
}

#[derive(Debug)]
pub enum OverlaysOutcome {
    UpToDate(ActiveOverlays),
    /// Check only: the server has a newer file, not downloaded.
    Available(crate::download::Newer),
    Activated {
        overlays: ActiveOverlays,
        replaced: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct Pointer {
    schema: u32,
    version: String,
}

/// The region's active overlays, if any.
pub fn active_overlays(
    store: &PackageStore,
    region_id: &str,
) -> Result<Option<ActiveOverlays>, PackageError> {
    let id = overlays_id(region_id);
    let text = match fs::read_to_string(store.region_dir(&id).join("active.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pointer: Pointer = serde_json::from_str(&text)
        .map_err(|e| PackageError::Invalid(format!("overlays pointer is unreadable: {e}")))?;
    if pointer.schema != 1 || !is_version_name(&pointer.version) {
        return Err(PackageError::Invalid("overlays pointer is invalid".into()));
    }
    let dir = store.version_dir(&id, &pointer.version);
    let record: OverlayRecord =
        serde_json::from_str(&fs::read_to_string(dir.join(RECORD_FILE))?)
            .map_err(|e| PackageError::Invalid(format!("overlays record is unreadable: {e}")))?;
    let path = dir.join(OVERLAYS_FILE);
    if record.schema != RECORD_SCHEMA
        || record.region_id != region_id
        || fs::metadata(&path).map(|m| m.len()).ok() != Some(record.byte_length)
    {
        return Err(PackageError::Invalid(
            "overlays do not match their record".into(),
        ));
    }
    Ok(Some(ActiveOverlays {
        version: pointer.version,
        path,
        record,
    }))
}

/// Removes every overlays version except the active one; returns how many
/// could not be removed yet (still open).
pub fn prune_overlays(store: &PackageStore, region_id: &str) -> usize {
    let Ok(Some(active)) = active_overlays(store, region_id) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(store.region_dir(&overlays_id(region_id)).join("versions"))
    else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != active.version)
        .filter(|e| fs::remove_dir_all(e.path()).is_err())
        .count()
}

/// Checks an overlay file and reads what its build recorded.
pub fn verify_overlays(
    path: &std::path::Path,
    region: &Region,
) -> Result<(Bounds, OverlaySources), PackageError> {
    let package = TilePackage::open(path)?;
    let metadata = package.metadata();
    if let Some(missing) = OVERLAY_LAYERS
        .iter()
        .find(|layer| !metadata.vector_layers.iter().any(|l| l == *layer))
    {
        return Err(PackageError::Invalid(format!(
            "layer {missing} is missing; this is not an overlay file"
        )));
    }
    if !region.outer.contains(&metadata.bounds) {
        return Err(PackageError::Invalid(format!(
            "the overlay file is not for {}",
            region.name
        )));
    }
    package.check()?;
    let bounds = metadata.bounds;
    drop(package);
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let value = |key: &str| -> Result<String, PackageError> {
        Ok(db
            .query_row(
                "SELECT value FROM metadata WHERE name = ?1",
                [format!("offline-us-maps:{key}")],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_default())
    };
    let sources = OverlaySources {
        flood_product: value("flood_product")?,
        flood_effective: value("flood_effective")?,
        public_lands: value("public_lands")?,
        built_at: value("built_at")?,
    };
    Ok((bounds, sources))
}

fn is_same_file(record: &OverlayRecord, url: &str, remote: &crate::download::RemoteInfo) -> bool {
    record.source.url == url
        && remote.length == Some(record.byte_length)
        && match (&remote.etag, &record.source.etag) {
            (Some(a), Some(b)) => a == b,
            _ => {
                remote.last_modified.is_some()
                    && remote.last_modified == record.source.last_modified
            }
        }
}

/// Downloads or updates a region's overlays from `url`.
pub fn update_overlays(
    store: &PackageStore,
    region: &Region,
    url: &str,
    transport: &dyn Transport,
    options: &UpdateOptions,
    progress: &mut dyn FnMut(Progress),
    cancelled: &AtomicBool,
) -> Result<OverlaysOutcome, UpdateError> {
    let id = overlays_id(region.id);
    let _lock = store.lock(&id).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => UpdateError::Busy,
        _ => UpdateError::Storage(e),
    })?;
    let installed = active_overlays(store, region.id).ok().flatten();
    progress(Progress::Checking);
    let remote = transport.probe(url).map_err(network)?;
    if let Some(active) = installed
        .as_ref()
        .filter(|a| is_same_file(&a.record, url, &remote))
    {
        return Ok(OverlaysOutcome::UpToDate(active.clone()));
    }
    if options.check_only {
        return Ok(OverlaysOutcome::Available(crate::download::Newer::of(
            &remote,
        )));
    }
    check_size(remote.length, options)?;
    let source = download_to_staging(
        store, &id, url, &remote, transport, options, progress, cancelled,
    )?;
    let part = store.staging_file(&id);
    if cancelled.load(Ordering::Relaxed) {
        return Err(UpdateError::Cancelled);
    }
    let downloaded_at = (options.clock)();
    progress(Progress::Verifying);
    let (sha256, byte_length) = sha256_file(&part, cancelled).map_err(|e| match e.kind() {
        io::ErrorKind::Interrupted => UpdateError::Cancelled,
        _ => UpdateError::Storage(e),
    })?;
    let (bounds, sources) = match verify_overlays(&part, region) {
        Ok(found) => found,
        Err(e) => {
            let _ = store.clear_staging(&id);
            return Err(UpdateError::Invalid(e));
        }
    };
    progress(Progress::Activating);
    let record = OverlayRecord {
        schema: RECORD_SCHEMA,
        region_id: region.id.to_owned(),
        source,
        byte_length,
        sha256: sha256.clone(),
        downloaded_at: downloaded_at.clone(),
        bounds,
        sources,
        attribution: OVERLAYS_ATTRIBUTION.to_owned(),
    };
    let stamp: String = downloaded_at
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let version = format!("{stamp}-{}", &sha256[..12]);
    let dir = store.version_dir(&id, &version);
    let install = || -> io::Result<()> {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fs::rename(&part, dir.join(OVERLAYS_FILE))?;
        let json = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
        write_atomic(&dir.join(RECORD_FILE), &json)?;
        let pointer = serde_json::to_vec(&Pointer {
            schema: 1,
            version: version.clone(),
        })
        .map_err(io::Error::other)?;
        write_atomic(&store.region_dir(&id).join("active.json"), &pointer)?;
        store.clear_staging(&id)
    };
    install().map_err(UpdateError::Storage)?;
    let overlays = active_overlays(store, region.id)
        .map_err(UpdateError::Invalid)?
        .ok_or_else(|| {
            UpdateError::Storage(io::Error::other("the overlays could not be read back"))
        })?;
    Ok(OverlaysOutcome::Activated {
        overlays,
        replaced: installed.map(|a| a.version),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::tests::{FakeTransport, options};
    use crate::mbtiles::tests::{build_package, valid_metadata};
    use crate::package::ILLINOIS;
    use crate::store::tests::TempDir;

    const LAYERS: &str = r#"{"vector_layers":[{"id":"flood_zones"},{"id":"public_lands"},{"id":"public_land_labels"}]}"#;

    fn overlay_file(marker: &[u8]) -> Vec<u8> {
        let mut metadata = valid_metadata();
        metadata.retain(|(k, _)| *k != "json");
        metadata.push(("json", LAYERS));
        metadata.push(("offline-us-maps:flood_product", "NFHL_17_20261027"));
        metadata.push(("offline-us-maps:flood_effective", "07/21/2026"));
        let file = build_package(&metadata, &[(0, 0, 0, marker), (1, 0, 1, b"b")]);
        fs::read(&file.0).unwrap()
    }

    #[test]
    fn installs_checks_and_replaces_overlays() {
        let dir = TempDir::new();
        let store = PackageStore::new(&dir.0);
        let url = overlays_url("https://example.test/releases/download/overlays/", "us-il");
        assert_eq!(
            url,
            "https://example.test/releases/download/overlays/us-il-overlays.mbtiles"
        );
        let first = FakeTransport::new(overlay_file(b"a"), "\"v1\"");
        let run = |t: &FakeTransport| {
            update_overlays(
                &store,
                &ILLINOIS,
                &url,
                t,
                &options(),
                &mut |_| {},
                &AtomicBool::new(false),
            )
        };
        let OverlaysOutcome::Activated { overlays, replaced } = run(&first).unwrap() else {
            panic!("expected overlays");
        };
        assert!(replaced.is_none());
        assert_eq!(overlays.record.sources.flood_product, "NFHL_17_20261027");
        assert_eq!(overlays.record.sources.flood_effective, "07/21/2026");
        assert!(matches!(run(&first).unwrap(), OverlaysOutcome::UpToDate(_)));

        let second = FakeTransport::new(overlay_file(b"c"), "\"v2\"");
        let OverlaysOutcome::Activated { replaced, .. } = run(&second).unwrap() else {
            panic!("expected new overlays");
        };
        assert_eq!(replaced.as_deref(), Some(overlays.version.as_str()));
        assert_eq!(prune_overlays(&store, "us-il"), 0);
        assert!(!overlays.path.exists());

        // A map package is not an overlay file.
        let package = crate::package::tests::shortbread_package_bytes(b"x");
        let wrong = FakeTransport::new(package, "\"v3\"");
        assert!(matches!(run(&wrong), Err(UpdateError::Invalid(_))));
    }
}
