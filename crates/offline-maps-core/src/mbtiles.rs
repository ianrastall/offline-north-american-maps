//! Read-only access to an MBTiles vector-tile package.
//!
//! The map renderer requests tiles in XYZ (north-up) order; MBTiles stores rows
//! in TMS (south-up) order. Tile bodies are returned exactly as stored so a
//! gzip-compressed tile can be passed to the renderer with
//! `Content-Encoding: gzip` instead of being decompressed and recompressed.
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Highest zoom level accepted in a tile request or package metadata.
pub const MAX_ZOOM: u8 = 22;

#[derive(Debug)]
pub enum PackageError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    /// The file opened but is not a usable map package.
    Invalid(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => write!(f, "Map package database error: {error}"),
            Self::Io(error) => write!(f, "Map package file error: {error}"),
            Self::Invalid(reason) => write!(f, "Invalid map package: {reason}"),
        }
    }
}

impl std::error::Error for PackageError {}

impl From<rusqlite::Error> for PackageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

impl From<std::io::Error> for PackageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

fn invalid(reason: impl Into<String>) -> PackageError {
    PackageError::Invalid(reason.into())
}

/// Geographic extent in degrees (WGS84), west/south/east/north.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Bounds {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

impl Bounds {
    pub fn new(west: f64, south: f64, east: f64, north: f64) -> Option<Self> {
        let finite = [west, south, east, north].iter().all(|v| v.is_finite());
        (finite
            && (-180.0..=180.0).contains(&west)
            && (-180.0..=180.0).contains(&east)
            && (-90.0..=90.0).contains(&south)
            && (-90.0..=90.0).contains(&north)
            && west < east
            && south < north)
            .then_some(Self {
                west,
                south,
                east,
                north,
            })
    }

    /// Parses the MBTiles `bounds` form: `west,south,east,north`.
    pub fn parse(text: &str) -> Option<Self> {
        let values: Vec<f64> = text
            .split(',')
            .map(|part| part.trim().parse().ok())
            .collect::<Option<_>>()?;
        match values[..] {
            [west, south, east, north] => Self::new(west, south, east, north),
            _ => None,
        }
    }

    pub fn contains(&self, other: &Bounds) -> bool {
        self.west <= other.west
            && self.south <= other.south
            && self.east >= other.east
            && self.north >= other.north
    }
}

/// Validated MBTiles metadata needed to render and describe a package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TileMetadata {
    pub name: String,
    pub version: Option<String>,
    pub format: String,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub bounds: Bounds,
    pub author: Option<String>,
    pub license: Option<String>,
    pub attribution: Option<String>,
    /// Vector layer ids declared in the `json` metadata entry.
    pub vector_layers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileEncoding {
    Gzip,
    Identity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub data: Vec<u8>,
    pub encoding: TileEncoding,
}

/// Result of a structural package check. A package must pass this before it is
/// recorded as ready; rendering can still fail for other reasons.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageCheck {
    pub tiles_per_zoom: BTreeMap<u8, u64>,
    pub total_tiles: u64,
    pub tile_bytes: u64,
}

pub struct TilePackage {
    connection: Connection,
    metadata: TileMetadata,
}

impl TilePackage {
    /// Opens a package read-only and validates its metadata.
    pub fn open(path: &Path) -> Result<Self, PackageError> {
        if !path.is_file() {
            return Err(invalid(format!("{} is not a file", path.display())));
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let metadata = read_metadata(&connection)?;
        Ok(Self {
            connection,
            metadata,
        })
    }

    pub fn metadata(&self) -> &TileMetadata {
        &self.metadata
    }

    /// Returns the tile at XYZ coordinates, or `None` when the package has no
    /// tile there (the renderer treats that as empty).
    pub fn tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Tile>, PackageError> {
        if z > MAX_ZOOM || u64::from(x) >= 1_u64 << z || u64::from(y) >= 1_u64 << z {
            return Err(invalid(format!("tile {z}/{x}/{y} is out of range")));
        }
        let row = (1_i64 << z) - 1 - i64::from(y);
        let data: Option<Vec<u8>> = self
            .connection
            .prepare_cached(
                "SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
            )?
            .query_row((z, x, row), |r| r.get(0))
            .optional()?;
        Ok(data.map(|data| {
            let encoding = if data.starts_with(&[0x1f, 0x8b]) {
                TileEncoding::Gzip
            } else {
                TileEncoding::Identity
            };
            Tile { data, encoding }
        }))
    }

    /// Number of tiles stored at `zoom`.
    pub fn tile_count(&self, zoom: u8) -> Result<u64, PackageError> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM tiles WHERE zoom_level = ?1",
            [zoom],
            |r| r.get(0),
        )?;
        Ok(count as u64)
    }

    /// Calls `visit(x, y, data)` (XYZ addressing) for every tile at `zoom`.
    /// Stops early when `visit` returns `Ok(false)`.
    pub fn for_each_tile(
        &self,
        zoom: u8,
        mut visit: impl FnMut(u32, u32, &[u8]) -> Result<bool, PackageError>,
    ) -> Result<(), PackageError> {
        let mut statement = self
            .connection
            .prepare("SELECT tile_column, tile_row, tile_data FROM tiles WHERE zoom_level = ?1")?;
        let mut rows = statement.query([zoom])?;
        let size = 1_i64 << zoom;
        while let Some(row) = rows.next()? {
            let (x, row_index): (i64, i64) = (row.get(0)?, row.get(1)?);
            if !(0..size).contains(&x) || !(0..size).contains(&row_index) {
                return Err(invalid(format!(
                    "tile {zoom}/{x}/{row_index} is out of range"
                )));
            }
            let data = row.get_ref(2)?.as_blob().map_err(rusqlite::Error::from)?;
            if !visit(x as u32, (size - 1 - row_index) as u32, data)? {
                break;
            }
        }
        Ok(())
    }

    /// Runs SQLite's integrity check and confirms tiles exist across the
    /// declared zoom range. This reads every page of the file.
    pub fn check(&self) -> Result<PackageCheck, PackageError> {
        let result: String = self
            .connection
            .query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if result != "ok" {
            return Err(invalid(format!("SQLite integrity check failed: {result}")));
        }
        let mut tiles_per_zoom = BTreeMap::new();
        let mut tile_bytes = 0_u64;
        let mut statement = self.connection.prepare(
            "SELECT zoom_level, COUNT(*), SUM(LENGTH(tile_data)) FROM tiles GROUP BY zoom_level",
        )?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (zoom, count, bytes) = row?;
            let zoom = u8::try_from(zoom)
                .ok()
                .filter(|z| *z <= MAX_ZOOM)
                .ok_or_else(|| invalid(format!("tile zoom level {zoom} is out of range")))?;
            tiles_per_zoom.insert(zoom, count as u64);
            tile_bytes += bytes as u64;
        }
        let (min, max) = (self.metadata.min_zoom, self.metadata.max_zoom);
        if let Some(zoom) = (min..=max).find(|z| !tiles_per_zoom.contains_key(z)) {
            return Err(invalid(format!("no tiles at declared zoom level {zoom}")));
        }
        if let Some(zoom) = tiles_per_zoom.keys().find(|z| **z < min || **z > max) {
            return Err(invalid(format!(
                "tiles at zoom {zoom} are outside declared range {min}-{max}"
            )));
        }
        Ok(PackageCheck {
            total_tiles: tiles_per_zoom.values().sum(),
            tiles_per_zoom,
            tile_bytes,
        })
    }
}

fn read_metadata(connection: &Connection) -> Result<TileMetadata, PackageError> {
    let table_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('metadata', 'tiles')",
        [],
        |r| r.get(0),
    )?;
    if table_count != 2 {
        return Err(invalid("missing MBTiles metadata or tiles table"));
    }
    let mut statement = connection.prepare("SELECT name, value FROM metadata")?;
    let entries: BTreeMap<String, String> = statement
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let get = |key: &str| entries.get(key).map(|v| v.trim().to_owned());
    let required = |key: &str| get(key).ok_or_else(|| invalid(format!("missing {key} metadata")));
    let zoom = |key: &str| -> Result<u8, PackageError> {
        required(key)?
            .parse::<u8>()
            .ok()
            .filter(|z| *z <= MAX_ZOOM)
            .ok_or_else(|| invalid(format!("{key} is not a zoom level")))
    };

    let format = required("format")?;
    if format != "pbf" {
        return Err(invalid(format!(
            "format is {format}, expected pbf vector tiles"
        )));
    }
    let (min_zoom, max_zoom) = (zoom("minzoom")?, zoom("maxzoom")?);
    if min_zoom > max_zoom {
        return Err(invalid("minzoom is greater than maxzoom"));
    }
    let bounds =
        Bounds::parse(&required("bounds")?).ok_or_else(|| invalid("bounds are invalid"))?;
    let vector_layers = match get("json") {
        Some(json) => {
            #[derive(Deserialize)]
            struct Layers {
                vector_layers: Vec<Layer>,
            }
            #[derive(Deserialize)]
            struct Layer {
                id: String,
            }
            serde_json::from_str::<Layers>(&json)
                .map_err(|e| invalid(format!("json metadata is invalid: {e}")))?
                .vector_layers
                .into_iter()
                .map(|l| l.id)
                .collect()
        }
        None => Vec::new(),
    };
    if vector_layers.is_empty() {
        return Err(invalid("no vector layers declared"));
    }
    Ok(TileMetadata {
        name: get("name").unwrap_or_default(),
        version: get("version"),
        format,
        min_zoom,
        max_zoom,
        bounds,
        author: get("author"),
        license: get("license"),
        attribution: get("attribution"),
        vector_layers,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A temporary file removed on drop.
    pub(crate) struct TempFile(pub PathBuf);
    impl TempFile {
        pub(crate) fn new(extension: &str) -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "offline-maps-test-{}-{n}.{extension}",
                std::process::id()
            )))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const LAYERS: &str = r#"{"vector_layers":[{"id":"streets"},{"id":"place_labels"}]}"#;

    pub(crate) fn build_package(
        metadata: &[(&str, &str)],
        tiles: &[(u8, u32, u32, &[u8])],
    ) -> TempFile {
        let file = TempFile::new("mbtiles");
        let db = Connection::open(&file.0).unwrap();
        db.execute_batch(
            "CREATE TABLE metadata (name TEXT, value TEXT);
             CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);",
        )
        .unwrap();
        for (name, value) in metadata {
            db.execute("INSERT INTO metadata VALUES (?1, ?2)", (name, value))
                .unwrap();
        }
        for (z, x, row, data) in tiles {
            db.execute(
                "INSERT INTO tiles VALUES (?1, ?2, ?3, ?4)",
                (z, x, row, data),
            )
            .unwrap();
        }
        file
    }

    pub(crate) fn valid_metadata() -> Vec<(&'static str, &'static str)> {
        vec![
            ("name", "Shortbread"),
            ("version", "1.0"),
            ("format", "pbf"),
            ("minzoom", "0"),
            ("maxzoom", "1"),
            ("bounds", "-91.5,36.9,-87.4,42.5"),
            ("license", "Open Database License 1.0"),
            ("json", LAYERS),
        ]
    }

    #[test]
    fn reads_metadata_and_flips_rows() {
        // At zoom 1, XYZ y=0 (north) is TMS row 1.
        let file = build_package(
            &valid_metadata(),
            &[
                (0, 0, 0, b"\x1f\x8bzero"),
                (1, 1, 1, b"north"),
                (1, 1, 0, b"south"),
            ],
        );
        let package = TilePackage::open(&file.0).unwrap();
        let meta = package.metadata();
        assert_eq!((meta.min_zoom, meta.max_zoom), (0, 1));
        assert_eq!(meta.vector_layers, ["streets", "place_labels"]);
        assert_eq!(meta.bounds, Bounds::new(-91.5, 36.9, -87.4, 42.5).unwrap());

        let north = package.tile(1, 1, 0).unwrap().unwrap();
        assert_eq!(
            (north.data.as_slice(), north.encoding),
            (&b"north"[..], TileEncoding::Identity)
        );
        assert_eq!(package.tile(1, 1, 1).unwrap().unwrap().data, b"south");
        assert_eq!(
            package.tile(0, 0, 0).unwrap().unwrap().encoding,
            TileEncoding::Gzip
        );
        assert!(package.tile(1, 0, 0).unwrap().is_none());
        assert!(package.tile(1, 2, 0).is_err());
        assert!(package.tile(23, 0, 0).is_err());

        let check = package.check().unwrap();
        assert_eq!(check.total_tiles, 3);
        assert_eq!(check.tiles_per_zoom[&1], 2);
        assert_eq!(check.tile_bytes, 6 + 5 + 5);
    }

    #[test]
    fn rejects_unusable_metadata() {
        let cases: [(&str, Option<&str>); 6] = [
            ("format", Some("png")),
            ("format", None),
            ("minzoom", Some("3")),
            ("maxzoom", Some("40")),
            ("bounds", Some("10,0,5,1")),
            ("json", Some("{}")),
        ];
        for (key, value) in cases {
            let mut metadata = valid_metadata();
            metadata.retain(|(k, _)| *k != key);
            if let Some(value) = value {
                metadata.push((key, value));
            }
            let file = build_package(&metadata, &[]);
            assert!(
                matches!(TilePackage::open(&file.0), Err(PackageError::Invalid(_))),
                "{key}={value:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_non_packages() {
        let file = TempFile::new("mbtiles");
        std::fs::write(&file.0, b"not a database").unwrap();
        assert!(TilePackage::open(&file.0).is_err());
        assert!(TilePackage::open(Path::new("Z:/missing/package.mbtiles")).is_err());
        let empty = TempFile::new("mbtiles");
        Connection::open(&empty.0)
            .unwrap()
            .execute_batch("CREATE TABLE other (x);")
            .unwrap();
        assert!(matches!(
            TilePackage::open(&empty.0),
            Err(PackageError::Invalid(_))
        ));
    }

    #[test]
    fn check_requires_tiles_across_declared_zooms() {
        let missing_zoom = build_package(&valid_metadata(), &[(0, 0, 0, b"t")]);
        assert!(TilePackage::open(&missing_zoom.0).unwrap().check().is_err());
        let extra_zoom = build_package(
            &valid_metadata(),
            &[(0, 0, 0, b"t"), (1, 0, 0, b"t"), (2, 0, 0, b"t")],
        );
        assert!(TilePackage::open(&extra_zoom.0).unwrap().check().is_err());
    }

    #[test]
    fn bounds_parse_and_contain() {
        assert!(Bounds::parse("-91.5, 36.9, -87.4, 42.5").is_some());
        for text in ["", "1,2,3", "a,b,c,d", "0,0,0,0", "-181,0,1,1", "0,NaN,1,1"] {
            assert!(Bounds::parse(text).is_none(), "{text}");
        }
        let outer = Bounds::new(-92.0, 36.0, -87.0, 43.0).unwrap();
        let inner = Bounds::new(-91.5, 36.9, -87.4, 42.5).unwrap();
        assert!(outer.contains(&inner) && !inner.contains(&outer));
    }
}
