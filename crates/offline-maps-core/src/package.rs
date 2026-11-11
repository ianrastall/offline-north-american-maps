//! Verification of a downloaded map package and the record kept with it.
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::mbtiles::{Bounds, PackageCheck, PackageError, TileMetadata, TilePackage};
use crate::{OSM_ATTRIBUTION, OSM_LICENSE};

pub const RECORD_SCHEMA: u32 = 1;

/// Vector layers the Offline US Maps style depends on.
pub const REQUIRED_LAYERS: [&str; 6] = [
    "streets",
    "street_labels",
    "place_labels",
    "water_polygons",
    "land",
    "boundaries",
];

/// A downloadable region and the coverage its package must have.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub id: &'static str,
    pub name: &'static str,
    pub source_page: &'static str,
    pub package_url: &'static str,
    /// Raw OpenStreetMap extract, read for what the package lacks (fuel and
    /// charging stations). Only the extracted stations are kept.
    pub extras_url: &'static str,
    /// Package bounds must lie inside this box.
    pub outer: Bounds,
    /// Package bounds must cover each of these (longitude, latitude) points.
    pub required_points: &'static [(f64, f64)],
    /// Approximate download sizes, for showing before a download (the files
    /// are rebuilt regularly; the server's figure is used while downloading).
    pub package_bytes: u64,
    pub extras_bytes: u64,
    /// Terrain tiles for the region's map area (see `terrain`).
    pub terrain_bytes: u64,
    /// The prebuilt flood zones and public lands file (see `overlays`); 0 when
    /// none is published for the region.
    pub overlay_bytes: u64,
}

pub const ILLINOIS: Region = Region {
    id: "us-il",
    name: "Illinois",
    source_page: crate::ILLINOIS_SOURCE_PAGE,
    package_url: "https://download.geofabrik.de/north-america/us/illinois-shortbread-1.0.mbtiles",
    extras_url: crate::ILLINOIS_OSM_PBF,
    outer: Bounds {
        west: -92.5,
        south: 36.0,
        east: -86.5,
        north: 43.5,
    },
    // Rockford, Chicago, Springfield, Cairo, Quincy.
    required_points: &[
        (-89.094, 42.271),
        (-87.630, 41.878),
        (-89.650, 39.781),
        (-89.176, 37.005),
        (-91.410, 39.936),
    ],
    package_bytes: 362_692_608,
    extras_bytes: 363_155_297,
    terrain_bytes: 365_559_838,
    overlay_bytes: 69_693_440,
};

/// What the server said about the file when it was downloaded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub url: String,
    pub etag: Option<String>,
    /// HTTP `Last-Modified`; the closest available proxy for the data date.
    pub last_modified: Option<String>,
}

/// Persisted alongside a verified package. Distinguishes the source date
/// (server `Last-Modified`), the download time and the verification time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageRecord {
    pub schema: u32,
    pub region_id: String,
    pub source: SourceInfo,
    pub downloaded_at: String,
    pub verified_at: String,
    pub byte_length: u64,
    pub sha256: String,
    pub attribution: String,
    pub license_url: String,
    pub metadata: TileMetadata,
    pub check: PackageCheck,
}

impl PackageRecord {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("package record serializes")
    }

    pub fn from_json(text: &str) -> Result<Self, PackageError> {
        let record: Self = serde_json::from_str(text)
            .map_err(|e| PackageError::Invalid(format!("package record is unreadable: {e}")))?;
        if record.schema != RECORD_SCHEMA {
            return Err(PackageError::Invalid(format!(
                "package record schema {} is not supported",
                record.schema
            )));
        }
        Ok(record)
    }
}

/// Streams a file through SHA-256, checking `cancelled` between reads.
pub fn sha256_file(path: &Path, cancelled: &AtomicBool) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    let mut total = 0_u64;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Hashing cancelled",
            ));
        }
        let count = match file.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total += count as u64;
    }
    Ok((hex(&hasher.finalize()), total))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks that a package opens, covers `region`, has the layers the style
/// needs and passes the structural check. Success makes the package eligible
/// for activation; it does not activate it.
pub fn verify_package(
    path: &Path,
    region: &Region,
    source: SourceInfo,
    downloaded_at: &str,
    verified_at: &str,
    cancelled: &AtomicBool,
) -> Result<PackageRecord, PackageError> {
    let (sha256, byte_length) = sha256_file(path, cancelled)?;
    let package = TilePackage::open(path)?;
    let metadata = package.metadata().clone();
    check_coverage(&metadata, region)?;
    if let Some(missing) = REQUIRED_LAYERS
        .iter()
        .find(|layer| !metadata.vector_layers.iter().any(|l| l == *layer))
    {
        return Err(PackageError::Invalid(format!(
            "vector layer {missing} is missing; this is not a Shortbread package"
        )));
    }
    let check = package.check()?;
    Ok(PackageRecord {
        schema: RECORD_SCHEMA,
        region_id: region.id.to_owned(),
        source,
        downloaded_at: downloaded_at.to_owned(),
        verified_at: verified_at.to_owned(),
        byte_length,
        sha256,
        attribution: OSM_ATTRIBUTION.to_owned(),
        license_url: OSM_LICENSE.to_owned(),
        metadata,
        check,
    })
}

fn check_coverage(metadata: &TileMetadata, region: &Region) -> Result<(), PackageError> {
    let b = metadata.bounds;
    if !region.outer.contains(&b) {
        return Err(PackageError::Invalid(format!(
            "package bounds extend outside {}",
            region.name
        )));
    }
    let covered = |(lon, lat): &(f64, f64)| {
        (b.west..=b.east).contains(lon) && (b.south..=b.north).contains(lat)
    };
    if !region.required_points.iter().all(covered) {
        return Err(PackageError::Invalid(format!(
            "package does not cover all of {}",
            region.name
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mbtiles::tests::{TempFile, build_package, valid_metadata};

    /// Bytes of a minimal valid Illinois Shortbread package; `marker` varies the content.
    pub(crate) fn shortbread_package_bytes(marker: &[u8]) -> Vec<u8> {
        use crate::mvt::tests::encode_layer;
        let name = String::from_utf8_lossy(marker).into_owned();
        let place = |x| {
            encode_layer(
                "place_labels",
                &[(&[("name", name.as_str()), ("kind", "town")], 1, &[(x, 100)])],
            )
        };
        let file = build_package(
            &metadata_with("json", SHORTBREAD_LAYERS),
            &[(0, 0, 0, &place(10)), (1, 0, 1, &place(20))],
        );
        std::fs::read(&file.0).unwrap()
    }

    const SHORTBREAD_LAYERS: &str = r#"{"vector_layers":[{"id":"streets"},{"id":"street_labels"},{"id":"place_labels"},{"id":"water_polygons"},{"id":"land"},{"id":"boundaries"}]}"#;

    fn metadata_with(key: &'static str, value: &'static str) -> Vec<(&'static str, &'static str)> {
        let mut m = valid_metadata();
        m.retain(|(k, _)| *k != key);
        m.push((key, value));
        m
    }

    fn verify(file: &TempFile) -> Result<PackageRecord, PackageError> {
        verify_package(
            &file.0,
            &ILLINOIS,
            SourceInfo {
                url: ILLINOIS.package_url.into(),
                etag: Some("\"abc\"".into()),
                last_modified: Some("Sun, 08 Nov 2026 04:12:34 GMT".into()),
            },
            "2026-11-08T06:57:00Z",
            "2026-11-08T07:00:00Z",
            &AtomicBool::new(false),
        )
    }

    #[test]
    fn hashes_known_content() {
        let file = TempFile::new("bin");
        std::fs::write(&file.0, b"abc").unwrap();
        let (hash, len) = sha256_file(&file.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(len, 3);
        let cancelled = sha256_file(&file.0, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(cancelled.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn verifies_and_records_a_package() {
        let file = build_package(
            &metadata_with("json", SHORTBREAD_LAYERS),
            &[(0, 0, 0, b"a"), (1, 0, 1, b"b")],
        );
        let record = verify(&file).unwrap();
        assert_eq!(record.region_id, "us-il");
        assert_eq!(record.sha256.len(), 64);
        assert_eq!(
            record.byte_length,
            std::fs::metadata(&file.0).unwrap().len()
        );
        assert_eq!(record.check.total_tiles, 2);
        assert_eq!(record.attribution, OSM_ATTRIBUTION);
        let round_trip = PackageRecord::from_json(&record.to_json()).unwrap();
        assert_eq!(round_trip, record);
    }

    #[test]
    fn rejects_wrong_region_layers_and_schema() {
        let tiles: &[(u8, u32, u32, &[u8])] = &[(0, 0, 0, b"a"), (1, 0, 1, b"b")];
        // Missing Shortbread layers (valid_metadata declares only two).
        assert!(verify(&build_package(&valid_metadata(), tiles)).is_err());
        let mut chicago_only = metadata_with("bounds", "-88.0,41.6,-87.5,42.1");
        chicago_only.push(("json", SHORTBREAD_LAYERS));
        assert!(verify(&build_package(&chicago_only, tiles)).is_err());
        let mut whole_us = metadata_with("bounds", "-125,24,-66,49");
        whole_us.push(("json", SHORTBREAD_LAYERS));
        assert!(verify(&build_package(&whole_us, tiles)).is_err());

        let record = verify(&build_package(
            &metadata_with("json", SHORTBREAD_LAYERS),
            tiles,
        ))
        .unwrap();
        let future = record.to_json().replace("\"schema\": 1", "\"schema\": 2");
        assert!(PackageRecord::from_json(&future).is_err());
        assert!(PackageRecord::from_json("{").is_err());
    }
}
