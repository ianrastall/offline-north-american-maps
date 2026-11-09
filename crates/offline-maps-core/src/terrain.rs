//! Terrain for a region: Mapterhorn's terrain tiles (Terrarium-encoded
//! elevation in 512-pixel WebP images) cut from its planet archive to the
//! region's map bounds, up to zoom 12 (about 15 m per pixel at 40° N), and kept
//! as MBTiles beside the region's map. The map page draws hillshading and
//! contour lines from them.
//!
//! The archive is 355 GB; only its index and the area's tiles are read, with
//! HTTP range requests. Neighbouring tiles are fetched together where they lie
//! close in the file. Every request after the first sends `If-Range`, so a
//! file replaced on the server mid-download is detected instead of mixed in.
//! A cancelled download keeps what it has and continues next time if the
//! server's file is unchanged.
//!
//! ```text
//! <root>/<region>/terrain/active.json
//! <root>/<region>/terrain/versions/<v>/terrain.mbtiles
//! <root>/<region>/terrain/versions/<v>/record.json
//! <root>/<region>/terrain/staging/terrain.part.mbtiles   download in progress
//! <root>/<region>/terrain/staging/staging.json           what it is a copy of
//! ```
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::download::{Progress, Transport, UpdateError, UpdateOptions};
use crate::mbtiles::{Bounds, PackageError};
use crate::package::{Region, SourceInfo};
use crate::pmtiles::{COMPRESSION_NONE, Header, Index, TILE_TYPE_WEBP};
use crate::store::{PackageStore, RECORD_FILE, is_version_name, write_atomic};

pub const TERRAIN_URL: &str = "https://download.mapterhorn.com/planet.pmtiles";
pub const TERRAIN_MAX_ZOOM: u8 = 12;
pub const TERRAIN_FILE: &str = "terrain.mbtiles";
pub const TERRAIN_ATTRIBUTION: &str = "Terrain: Mapterhorn (mapterhorn.com/attribution), including USGS 3DEP and Copernicus GLO-30 (© DLR e.V. 2010–2014 and © Airbus Defence and Space GmbH 2014–2018, provided under COPERNICUS by the European Union and ESA)";
const RECORD_SCHEMA: u32 = 1;
/// Tiles closer than this in the archive are fetched in one request.
const MAX_GAP: u64 = 512 << 10;
/// Largest single request.
const MAX_SPAN: u64 = 16 << 20;
/// Hard cap on one region's terrain (Texas is about 1.9 GB).
const MAX_BYTES: u64 = 4 << 30;

/// The store ID under which a region's terrain is kept (`us-il/terrain`).
pub fn terrain_id(region_id: &str) -> String {
    format!("{region_id}/terrain")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerrainRecord {
    pub schema: u32,
    pub region_id: String,
    pub source: SourceInfo,
    /// Size of the whole remote archive when downloaded.
    pub archive_length: Option<u64>,
    pub bounds: Bounds,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub tiles: u64,
    /// Bytes of tile data kept.
    pub bytes: u64,
    pub downloaded_at: String,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveTerrain {
    pub version: String,
    pub path: PathBuf,
    pub record: TerrainRecord,
}

#[derive(Debug)]
pub enum TerrainOutcome {
    UpToDate(ActiveTerrain),
    /// Check only: the server has a newer file (or the map's area changed),
    /// not downloaded. The size is not known until the tiles are listed.
    Available(crate::download::Newer),
    Activated {
        terrain: ActiveTerrain,
        replaced: Option<String>,
    },
}

#[derive(Serialize, Deserialize)]
struct Pointer {
    schema: u32,
    version: String,
}

/// What `staging/terrain.part.mbtiles` is a partial copy of.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Staging {
    url: String,
    validator: String,
    bounds: Bounds,
    max_zoom: u8,
}

/// The region's active terrain, if any.
pub fn active_terrain(
    store: &PackageStore,
    region_id: &str,
) -> Result<Option<ActiveTerrain>, PackageError> {
    let id = terrain_id(region_id);
    let text = match fs::read_to_string(store.region_dir(&id).join("active.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pointer: Pointer = serde_json::from_str(&text)
        .map_err(|e| PackageError::Invalid(format!("terrain pointer is unreadable: {e}")))?;
    if pointer.schema != 1 || !is_version_name(&pointer.version) {
        return Err(PackageError::Invalid("terrain pointer is invalid".into()));
    }
    let dir = store.version_dir(&id, &pointer.version);
    let record: TerrainRecord =
        serde_json::from_str(&fs::read_to_string(dir.join(RECORD_FILE))?)
            .map_err(|e| PackageError::Invalid(format!("terrain record is unreadable: {e}")))?;
    let path = dir.join(TERRAIN_FILE);
    if record.schema != RECORD_SCHEMA || record.region_id != region_id || !path.is_file() {
        return Err(PackageError::Invalid(
            "terrain does not match its record".into(),
        ));
    }
    Ok(Some(ActiveTerrain {
        version: pointer.version,
        path,
        record,
    }))
}

/// Removes every terrain version except the active one; returns how many
/// could not be removed yet (still open).
pub fn prune_terrain(store: &PackageStore, region_id: &str) -> usize {
    let Ok(Some(active)) = active_terrain(store, region_id) else {
        return 0;
    };
    let Ok(entries) = fs::read_dir(store.region_dir(&terrain_id(region_id)).join("versions"))
    else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != active.version)
        .filter(|e| fs::remove_dir_all(e.path()).is_err())
        .count()
}

/// Read-only access to an installed terrain file.
pub struct TerrainTiles {
    db: Connection,
}

impl TerrainTiles {
    pub fn open(path: &Path) -> Result<Self, PackageError> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(Self { db })
    }

    /// The WebP tile at z/x/y (XYZ numbering), if the file has it.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, PackageError> {
        let Some(row) = (1_u32 << z).checked_sub(1).and_then(|m| m.checked_sub(y)) else {
            return Ok(None);
        };
        Ok(self
            .db
            .prepare_cached(
                "SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
            )?
            .query_row(params![z, x, row], |r| r.get(0))
            .optional()?)
    }
}

fn storage(e: impl std::fmt::Display) -> UpdateError {
    UpdateError::Storage(io::Error::other(e.to_string()))
}

/// The XYZ tiles of zoom `z` that touch `b`.
pub fn tile_range(z: u8, b: &Bounds) -> (u32, u32, u32, u32) {
    let n = f64::from(1_u32 << z);
    let max = (1_u32 << z) - 1;
    let tx = |lon: f64| (((lon + 180.0) / 360.0 * n).floor().max(0.0) as u32).min(max);
    let ty = |lat: f64| {
        let r = lat.clamp(-85.05112878, 85.05112878).to_radians();
        (((1.0 - r.tan().asinh() / std::f64::consts::PI) / 2.0 * n)
            .floor()
            .max(0.0) as u32)
            .min(max)
    };
    (tx(b.west), tx(b.east), ty(b.north), ty(b.south))
}

fn create_staging_db(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    db.execute_batch(
        "PRAGMA journal_mode = DELETE;
         CREATE TABLE IF NOT EXISTS metadata (name TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE IF NOT EXISTS tiles (
             zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB,
             PRIMARY KEY (zoom_level, tile_column, tile_row));",
    )?;
    Ok(db)
}

/// One tile wanted from the archive (XYZ numbering).
#[derive(Clone, Copy)]
struct Wanted {
    z: u8,
    x: u32,
    y: u32,
}

/// Downloads or updates the terrain of `region` for the map area `bounds`.
#[allow(clippy::too_many_arguments)]
pub fn update_terrain(
    store: &PackageStore,
    region: &Region,
    bounds: Bounds,
    url: &str,
    max_zoom: u8,
    transport: &dyn Transport,
    options: &UpdateOptions,
    progress: &mut dyn FnMut(Progress),
    cancelled: &AtomicBool,
) -> Result<TerrainOutcome, UpdateError> {
    let id = terrain_id(region.id);
    let _lock = store.lock(&id).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => UpdateError::Busy,
        _ => UpdateError::Storage(e),
    })?;
    let check = || {
        if cancelled.load(Ordering::Relaxed) {
            Err(UpdateError::Cancelled)
        } else {
            Ok(())
        }
    };
    progress(Progress::Checking);
    let (info, header_bytes) = transport
        .fetch_range(url, 0, crate::pmtiles::HEADER_LENGTH, None)
        .map_err(UpdateError::Network)?;
    let header = Header::parse(&header_bytes).map_err(|e| {
        UpdateError::Invalid(PackageError::Invalid(format!("terrain archive: {e}")))
    })?;
    let validator = info
        .strong_etag()
        .map(str::to_owned)
        .or_else(|| info.last_modified.clone())
        .ok_or_else(|| {
            UpdateError::Network(io::Error::other(
                "the terrain server gave no version of its file",
            ))
        })?;
    let installed = active_terrain(store, region.id).ok().flatten();
    if let Some(active) = &installed
        && active.record.source.url == url
        && active.record.max_zoom == max_zoom
        && active.record.bounds == bounds
        && (active.record.source.etag.as_deref() == Some(validator.as_str())
            || active.record.source.last_modified.as_deref() == Some(validator.as_str()))
    {
        return Ok(TerrainOutcome::UpToDate(active.clone()));
    }
    if options.check_only {
        return Ok(TerrainOutcome::Available(crate::download::Newer {
            last_modified: info.last_modified.clone(),
            length: None,
        }));
    }

    let fetch = |offset: u64, length: u64| {
        transport
            .fetch_range(url, offset, length, Some(&validator))
            .map(|(_, bytes)| bytes)
    };
    let mut index = Index::with_header(header.clone(), fetch).map_err(UpdateError::Network)?;
    if header.tile_type != TILE_TYPE_WEBP || header.tile_compression != COMPRESSION_NONE {
        return Err(UpdateError::Invalid(PackageError::Invalid(
            "the terrain archive is not uncompressed WebP tiles".into(),
        )));
    }
    let max_zoom = max_zoom.min(header.max_zoom);
    // Tiles grouped by their place in the archive (runs share one copy).
    let mut by_location: BTreeMap<(u64, u32), Vec<Wanted>> = BTreeMap::new();
    for z in header.min_zoom..=max_zoom {
        check()?;
        let (x0, x1, y0, y1) = tile_range(z, &bounds);
        for x in x0..=x1 {
            for y in y0..=y1 {
                if let Some(location) = index.locate(z, x, y).map_err(UpdateError::Network)? {
                    by_location
                        .entry(location)
                        .or_default()
                        .push(Wanted { z, x, y });
                }
            }
        }
    }
    drop(index);
    let tiles: u64 = by_location.values().map(|v| v.len() as u64).sum();
    let total: u64 = by_location.keys().map(|(_, l)| u64::from(*l)).sum();
    if tiles == 0 {
        return Err(UpdateError::Invalid(PackageError::Invalid(
            "the terrain archive has no tiles for this region".into(),
        )));
    }
    if total > MAX_BYTES {
        return Err(UpdateError::TooLarge {
            size: total,
            limit: MAX_BYTES,
        });
    }

    let staging_dir = store.staging_dir(&id);
    fs::create_dir_all(&staging_dir).map_err(UpdateError::Storage)?;
    let available = (options.available_space)(&staging_dir).map_err(UpdateError::Storage)?;
    let needed = total + options.reserve_bytes;
    if available < needed {
        return Err(UpdateError::InsufficientSpace { needed, available });
    }
    let staging = Staging {
        url: url.to_owned(),
        validator: validator.clone(),
        bounds,
        max_zoom,
    };
    let db_path = staging_dir.join("terrain.part.mbtiles");
    let state_path = staging_dir.join("staging.json");
    let resumable = fs::read_to_string(&state_path)
        .ok()
        .and_then(|t| serde_json::from_str::<Staging>(&t).ok())
        .is_some_and(|s| s == staging)
        && db_path.is_file();
    if !resumable {
        let _ = fs::remove_file(&db_path);
        let json = serde_json::to_vec(&staging).map_err(storage)?;
        write_atomic(&state_path, &json).map_err(UpdateError::Storage)?;
    }
    let mut db = create_staging_db(&db_path).map_err(storage)?;
    let mut have = HashSet::new();
    {
        let mut statement = db
            .prepare("SELECT zoom_level, tile_column, tile_row FROM tiles")
            .map_err(storage)?;
        let rows = statement
            .query_map([], |r| {
                Ok((r.get::<_, u8>(0)?, r.get::<_, u32>(1)?, r.get::<_, u32>(2)?))
            })
            .map_err(storage)?;
        for row in rows {
            let (z, x, row) = row.map_err(storage)?;
            have.insert((z, x, (1_u32 << z) - 1 - row));
        }
    }
    let missing: Vec<((u64, u32), Vec<Wanted>)> = by_location
        .into_iter()
        .filter(|(_, wanted)| wanted.iter().any(|w| !have.contains(&(w.z, w.x, w.y))))
        .collect();
    let mut received = total - missing.iter().map(|((_, l), _)| u64::from(*l)).sum::<u64>();
    progress(Progress::Downloading {
        received,
        total: Some(total),
    });

    // Join tiles that lie close together into one request.
    let mut ranges: Vec<(u64, u64, Vec<usize>)> = Vec::new();
    for (i, ((offset, length), _)) in missing.iter().enumerate() {
        let end = offset + u64::from(*length);
        match ranges.last_mut() {
            Some((start, stop, members))
                if *offset <= *stop + MAX_GAP && end - *start <= MAX_SPAN =>
            {
                *stop = (*stop).max(end);
                members.push(i);
            }
            _ => ranges.push((*offset, end, vec![i])),
        }
    }
    for (start, stop, members) in ranges {
        check()?;
        let mut attempt = 0;
        let bytes = loop {
            match transport.fetch_range(url, start, stop - start, Some(&validator)) {
                Ok((_, bytes)) => break bytes,
                Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                    // The archive changed on the server: start over next time.
                    let _ = fs::remove_file(&state_path);
                    return Err(UpdateError::Network(io::Error::other(
                        "the terrain file changed on the server during the download; try again",
                    )));
                }
                Err(e) if attempt < 3 => {
                    attempt += 1;
                    let _ = e;
                    std::thread::sleep(std::time::Duration::from_secs(attempt));
                }
                Err(e) => return Err(UpdateError::Network(e)),
            }
        };
        let tx = db.transaction().map_err(storage)?;
        {
            let mut insert = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO tiles (zoom_level, tile_column, tile_row, tile_data)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(storage)?;
            for i in members {
                let ((offset, length), wanted) = &missing[i];
                let at = usize::try_from(offset - start).map_err(storage)?;
                let data = &bytes[at..at + *length as usize];
                for w in wanted {
                    insert
                        .execute(params![w.z, w.x, (1_u32 << w.z) - 1 - w.y, data])
                        .map_err(storage)?;
                }
                received += u64::from(*length);
            }
        }
        tx.commit().map_err(storage)?;
        progress(Progress::Downloading {
            received,
            total: Some(total),
        });
    }

    progress(Progress::Verifying);
    let count: i64 = db
        .query_row("SELECT count(*) FROM tiles", [], |r| r.get(0))
        .map_err(storage)?;
    let count = u64::try_from(count).map_err(storage)?;
    if count != tiles {
        return Err(UpdateError::Invalid(PackageError::Invalid(format!(
            "the terrain download has {count} tiles instead of {tiles}"
        ))));
    }
    let downloaded_at = (options.clock)();
    {
        let tx = db.transaction().map_err(storage)?;
        for (name, value) in [
            ("name", format!("{} terrain", region.name)),
            ("format", "webp".into()),
            ("encoding", "terrarium".into()),
            ("minzoom", header.min_zoom.to_string()),
            ("maxzoom", max_zoom.to_string()),
            (
                "bounds",
                format!(
                    "{},{},{},{}",
                    bounds.west, bounds.south, bounds.east, bounds.north
                ),
            ),
            ("attribution", TERRAIN_ATTRIBUTION.into()),
        ] {
            tx.execute(
                "INSERT OR REPLACE INTO metadata (name, value) VALUES (?1, ?2)",
                params![name, value],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
    }
    drop(db);

    progress(Progress::Activating);
    let record = TerrainRecord {
        schema: RECORD_SCHEMA,
        region_id: region.id.to_owned(),
        source: SourceInfo {
            url: url.to_owned(),
            etag: info.strong_etag().map(str::to_owned),
            last_modified: info.last_modified.clone(),
        },
        archive_length: info.length,
        bounds,
        min_zoom: header.min_zoom,
        max_zoom,
        tiles,
        bytes: total,
        downloaded_at: downloaded_at.clone(),
        attribution: TERRAIN_ATTRIBUTION.to_owned(),
    };
    let stamp: String = downloaded_at
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let digest = Sha256::digest(format!("{validator}{bounds:?}{max_zoom}").as_bytes());
    let hash: String = digest.iter().take(6).map(|b| format!("{b:02x}")).collect();
    let version = format!("{stamp}-{hash}");
    let dir = store.version_dir(&id, &version);
    let install = || -> io::Result<()> {
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        fs::rename(&db_path, dir.join(TERRAIN_FILE))?;
        let json = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
        write_atomic(&dir.join(RECORD_FILE), &json)?;
        let pointer = serde_json::to_vec(&Pointer {
            schema: 1,
            version: version.clone(),
        })
        .map_err(io::Error::other)?;
        write_atomic(&store.region_dir(&id).join("active.json"), &pointer)?;
        let _ = fs::remove_file(&state_path);
        Ok(())
    };
    install().map_err(UpdateError::Storage)?;
    let terrain = active_terrain(store, region.id)
        .map_err(UpdateError::Invalid)?
        .ok_or_else(|| storage("the installed terrain could not be read back"))?;
    Ok(TerrainOutcome::Activated {
        terrain,
        replaced: installed.map(|a| a.version),
    })
}

#[cfg(test)]
#[path = "terrain_tests.rs"]
mod tests;
