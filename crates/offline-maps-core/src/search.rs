//! Offline place search built from a map package.
//!
//! The index is a separate SQLite file (FTS5) derived from the package's
//! zoom-14 tiles: places, named streets, points of interest (including named
//! parks), stations and airports, and water. Streets and points of interest
//! carry the nearest town as context, so "main st clinton" finds Main Street
//! near Clinton. Nothing here touches the network.
//!
//! Geofabrik's Shortbread package has house numbers without street names, no
//! county labels and no names on land areas, so address and county search are
//! not possible from it. Addresses, fuel and charging stations come from the
//! extras database (see `extras` and `addresses`) when one is attached.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use crate::Coordinates;
use crate::mbtiles::{PackageError, TilePackage};
use crate::mvt::{Feature, decode_tile, tile_point_to_lon_lat};

pub const SEARCH_SCHEMA: u32 = 2;
pub const SEARCH_FILE: &str = "search.sqlite";
const LAYERS: [&str; 6] = [
    "place_labels",
    "street_labels",
    "pois",
    "public_transport",
    "water_polygons_labels",
    "water_lines_labels",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Place,
    Street,
    Poi,
    Transit,
    Water,
    /// A house number from the raw extract (see `addresses`).
    Address,
}

impl EntryKind {
    /// The stored and reported name: `place`, `street`, `poi`, `transit`, `water`, `address`.
    pub fn code(self) -> &'static str {
        match self {
            Self::Place => "place",
            Self::Street => "street",
            Self::Poi => "poi",
            Self::Transit => "transit",
            Self::Water => "water",
            Self::Address => "address",
        }
    }
    fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "place" => Self::Place,
            "street" => Self::Street,
            "poi" => Self::Poi,
            "transit" => Self::Transit,
            "water" => Self::Water,
            "address" => Self::Address,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub name: String,
    pub kind: EntryKind,
    /// OSM kind, e.g. `town`, `hospital`, `primary`, `lake`.
    pub category: String,
    /// Nearest town for streets and points of interest.
    pub near: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
    /// Route number(s) for roads, e.g. `US 51`.
    pub reference: Option<String>,
    /// Distance from the `near` point given to the search, if any.
    pub distance_km: Option<f64>,
    /// Extra facts for stations: brand, hours, fuels, address.
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexStats {
    pub tiles: u64,
    pub places: u64,
    pub streets: u64,
    pub pois: u64,
    pub transit: u64,
    pub water: u64,
    #[serde(default)]
    pub addresses: u64,
}

impl IndexStats {
    pub fn entries(&self) -> u64 {
        self.places + self.streets + self.pois + self.transit + self.water + self.addresses
    }
}

/// One searchable thing before context is assigned.
pub(crate) struct Entry {
    pub(crate) name: String,
    pub(crate) alt_name: Option<String>,
    pub(crate) reference: Option<String>,
    pub(crate) kind: EntryKind,
    pub(crate) category: String,
    pub(crate) lon: f64,
    pub(crate) lat: f64,
    pub(crate) tier: u8,
    pub(crate) population: u64,
    pub(crate) detail: Option<String>,
}

/// A piece of a line feature (street, river) or a park label. Pieces with the
/// same name are grouped by nearest town into one entry each, placed at the
/// piece nearest the group's centre so the marker lies on the feature.
struct Segment {
    lon: f64,
    lat: f64,
    tier: u8,
    category: String,
    alt_name: Option<String>,
    reference: Option<String>,
}

fn cell(lon: f64, lat: f64, per_degree: f64) -> (i32, i32) {
    (
        (lon * per_degree).floor() as i32,
        (lat * per_degree).floor() as i32,
    )
}

fn place_tier(kind: &str) -> Option<u8> {
    Some(match kind {
        "capital" | "state_capital" | "city" => 0,
        "town" => 1,
        "village" => 2,
        "hamlet" | "suburb" => 3,
        "neighbourhood" | "quarter" | "locality" | "island" => 4,
        "isolated_dwelling" | "farm" => 5,
        _ => return None,
    })
}

fn street_tier(kind: &str) -> u8 {
    match kind {
        "motorway" | "trunk" | "primary" => 3,
        "secondary" | "tertiary" | "motorway_link" | "trunk_link" | "primary_link" => 4,
        _ => 5,
    }
}

pub(crate) const EMERGENCY: [&str; 10] = [
    "hospital",
    "clinic",
    "doctors",
    "pharmacy",
    "police",
    "fire_station",
    "shelter",
    "fuel",
    "ambulance_station",
    "charging_station",
];

/// Words a category is found by: its own words plus common synonyms.
fn category_words(category: &str) -> String {
    match category {
        "fuel" => "fuel gas station petrol".into(),
        "charging_station" => "charging station ev charger electric vehicle".into(),
        other => other.replace('_', " "),
    }
}

fn poi_category(feature: &Feature) -> Option<String> {
    [
        "amenity",
        "emergency",
        "shop",
        "tourism",
        "leisure",
        "historic",
        "man_made",
        "information",
    ]
    .iter()
    .find_map(|key| feature.str(key))
    .filter(|v| !v.is_empty() && *v != "yes")
    .map(str::to_owned)
}

fn names(feature: &Feature) -> Option<(String, Option<String>)> {
    let name = feature
        .str("name")
        .map(str::trim)
        .filter(|n| !n.is_empty())?;
    let alt = feature
        .str("name_en")
        .map(str::trim)
        .filter(|n| !n.is_empty() && *n != name)
        .map(str::to_owned);
    Some((name.to_owned(), alt))
}

/// Builds the index for `package` at `out`. Writes to a temporary file and
/// renames it, so `out` is either absent, the previous index, or complete.
pub fn build_index(
    package: &TilePackage,
    package_sha256: &str,
    out: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<IndexStats, PackageError> {
    let zoom = package.metadata().max_zoom;
    let total = package.tile_count(zoom)?;
    let mut stats = IndexStats {
        tiles: total,
        ..IndexStats::default()
    };
    let mut points: Vec<Entry> = Vec::new();
    let mut seen_points: HashMap<(EntryKind, String, (i32, i32)), ()> = HashMap::new();
    let mut segments: HashMap<(EntryKind, String), Vec<Segment>> = HashMap::new();
    let mut done = 0;

    package.for_each_tile(zoom, |x, y, data| {
        if cancelled.load(Ordering::Relaxed) {
            return Err(PackageError::Io(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "Indexing cancelled",
            )));
        }
        let layers = decode_tile(data, &LAYERS)
            .map_err(|e| PackageError::Invalid(format!("tile {zoom}/{x}/{y}: {e}")))?;
        for layer in layers {
            let extent = layer.extent as i32;
            for feature in &layer.features {
                let Some((px, py)) = feature.point else {
                    continue;
                };
                let Some((name, alt_name)) = names(feature) else {
                    continue;
                };
                let inside = (0..extent).contains(&px) && (0..extent).contains(&py);
                let (lon, lat) = tile_point_to_lon_lat(zoom, x, y, layer.extent, px, py);
                let kind_value = feature.str("kind").unwrap_or("").to_owned();
                // Points are repeated in neighbouring tiles' buffers; keep the
                // copy inside the tile's own extent.
                let mut point = |kind: EntryKind, category: String, tier: u8, population: u64| {
                    if !inside {
                        return;
                    }
                    let key = (kind, name.to_lowercase(), cell(lon, lat, 1000.0));
                    if seen_points.insert(key, ()).is_none() {
                        points.push(Entry {
                            name: name.clone(),
                            alt_name: alt_name.clone(),
                            reference: None,
                            kind,
                            category,
                            lon,
                            lat,
                            tier,
                            population,
                            detail: None,
                        });
                    }
                };
                match layer.name.as_str() {
                    "place_labels" => {
                        if let Some(tier) = place_tier(&kind_value) {
                            let population = feature
                                .properties
                                .get("population")
                                .and_then(|v| v.as_f64())
                                .unwrap_or(0.0)
                                .max(0.0) as u64;
                            point(EntryKind::Place, kind_value, tier, population);
                        }
                    }
                    "pois" => {
                        if let Some(category) = poi_category(feature) {
                            let tier = if EMERGENCY.contains(&category.as_str()) {
                                2
                            } else {
                                4
                            };
                            point(EntryKind::Poi, category, tier, 0);
                        }
                    }
                    "public_transport" => point(EntryKind::Transit, kind_value, 3, 0),
                    "street_labels" | "water_polygons_labels" | "water_lines_labels" => {
                        let (kind, tier) = match layer.name.as_str() {
                            "street_labels" => (EntryKind::Street, street_tier(&kind_value)),
                            _ => (EntryKind::Water, 3),
                        };
                        segments
                            .entry((kind, name.clone()))
                            .or_default()
                            .push(Segment {
                                lon,
                                lat,
                                tier,
                                category: kind_value.clone(),
                                alt_name: alt_name.clone(),
                                reference: feature
                                    .str("ref")
                                    .filter(|r| !r.is_empty())
                                    .map(str::to_owned),
                            });
                    }
                    _ => {}
                }
            }
        }
        done += 1;
        if done % 512 == 0 || done == total {
            progress(done, total);
        }
        Ok(true)
    })?;

    let towns = TownGrid::new(&points);
    for ((kind, name), pieces) in segments {
        let mut groups: HashMap<Option<usize>, Vec<Segment>> = HashMap::new();
        for piece in pieces {
            groups
                .entry(towns.nearest_index(piece.lon, piece.lat))
                .or_default()
                .push(piece);
        }
        for group in groups.into_values() {
            let n = group.len() as f64;
            let (clon, clat) = group
                .iter()
                .fold((0.0, 0.0), |(a, b), p| (a + p.lon / n, b + p.lat / n));
            let centre = group
                .iter()
                .min_by(|a, b| {
                    distance_km(clat, clon, a.lat, a.lon)
                        .total_cmp(&distance_km(clat, clon, b.lat, b.lon))
                })
                .expect("groups are non-empty");
            let main = group
                .iter()
                .min_by_key(|p| p.tier)
                .expect("groups are non-empty");
            points.push(Entry {
                name: name.clone(),
                alt_name: group.iter().find_map(|p| p.alt_name.clone()),
                reference: group.iter().find_map(|p| p.reference.clone()),
                kind,
                category: main.category.clone(),
                lon: centre.lon,
                lat: centre.lat,
                tier: main.tier,
                population: 0,
                detail: None,
            });
        }
    }
    write_index(&points, &towns, package_sha256, out, &mut stats)?;
    Ok(stats)
}

/// Longitude, latitude and index into `TownGrid::names`.
type TownPoint = (f64, f64, usize);

/// Nearest city/town/village/hamlet lookup over a coarse grid.
pub(crate) struct TownGrid {
    cells: HashMap<(i32, i32), Vec<TownPoint>>,
    names: Vec<String>,
}

const TOWN_CELLS_PER_DEGREE: f64 = 4.0;

impl TownGrid {
    fn new(entries: &[Entry]) -> Self {
        Self::from_points(
            entries
                .iter()
                .filter(|e| e.kind == EntryKind::Place && e.tier <= 3)
                .map(|e| (e.name.clone(), e.lon, e.lat)),
        )
    }

    /// From (name, longitude, latitude) of cities, towns, villages and hamlets.
    pub(crate) fn from_points(points: impl Iterator<Item = (String, f64, f64)>) -> Self {
        let mut grid = Self {
            cells: HashMap::new(),
            names: Vec::new(),
        };
        for (name, lon, lat) in points {
            grid.names.push(name);
            grid.cells
                .entry(cell(lon, lat, TOWN_CELLS_PER_DEGREE))
                .or_default()
                .push((lon, lat, grid.names.len() - 1));
        }
        grid
    }

    /// Nearest town within ~40 km.
    pub(crate) fn nearest(&self, lon: f64, lat: f64) -> Option<&str> {
        self.nearest_index(lon, lat).map(|i| self.names[i].as_str())
    }

    fn nearest_index(&self, lon: f64, lat: f64) -> Option<usize> {
        let (cx, cy) = cell(lon, lat, TOWN_CELLS_PER_DEGREE);
        let mut best: Option<(f64, usize)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for (tlon, tlat, i) in self.cells.get(&(cx + dx, cy + dy)).into_iter().flatten() {
                    let d = distance_km(lat, lon, *tlat, *tlon);
                    if d <= 40.0 && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, *i));
                    }
                }
            }
        }
        best.map(|(_, i)| i)
    }
}

/// Equirectangular approximation; accurate to well under 1% at state scale.
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let mean = ((lat1 + lat2) / 2.0).to_radians();
    let dx = (lon2 - lon1).to_radians() * mean.cos();
    let dy = (lat2 - lat1).to_radians();
    6371.0 * (dx * dx + dy * dy).sqrt()
}

/// Tables shared by the package index and the extras index.
pub(crate) const ENTRY_TABLES: &str = "
    CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE entries (
        id INTEGER PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
        category TEXT NOT NULL, near TEXT, lon REAL NOT NULL, lat REAL NOT NULL,
        tier INTEGER NOT NULL, population INTEGER NOT NULL, reference TEXT,
        match_name TEXT NOT NULL, match_ref TEXT NOT NULL, detail TEXT);
    CREATE INDEX entries_location ON entries (kind, lat, lon);
    CREATE VIRTUAL TABLE entries_fts USING fts5(
        terms, content = '', prefix = '2 3',
        tokenize = 'unicode61 remove_diacritics 2');";

/// Writes `entries` with their nearest town and search terms, counting them
/// in `stats`.
pub(crate) fn insert_entries(
    tx: &rusqlite::Transaction<'_>,
    entries: &[Entry],
    towns: &TownGrid,
    stats: &mut IndexStats,
) -> rusqlite::Result<()> {
    let mut insert = tx.prepare(
        "INSERT INTO entries (id, name, kind, category, near, lon, lat, tier, population,
                              reference, match_name, match_ref, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    let mut insert_terms = tx.prepare("INSERT INTO entries_fts (rowid, terms) VALUES (?1, ?2)")?;
    for (id, e) in entries.iter().enumerate() {
        let near = match e.kind {
            EntryKind::Place => None,
            _ => towns.nearest(e.lon, e.lat).filter(|t| *t != e.name),
        };
        let mut terms = expand(&e.name);
        let category = category_words(&e.category);
        for extra in [
            e.alt_name.as_deref(),
            e.reference.as_deref(),
            near,
            Some(category.as_str()),
        ]
        .into_iter()
        .flatten()
        {
            terms.push(' ');
            terms.push_str(&expand(extra));
        }
        insert.execute(params![
            id as i64,
            e.name,
            e.kind.code(),
            e.category,
            near,
            e.lon,
            e.lat,
            e.tier,
            e.population as i64,
            e.reference,
            canonical(&e.name),
            e.reference.as_deref().map(canonical).unwrap_or_default(),
            e.detail,
        ])?;
        insert_terms.execute(params![id as i64, terms])?;
        *match e.kind {
            EntryKind::Place => &mut stats.places,
            EntryKind::Street => &mut stats.streets,
            EntryKind::Poi => &mut stats.pois,
            EntryKind::Transit => &mut stats.transit,
            EntryKind::Water => &mut stats.water,
            EntryKind::Address => &mut stats.addresses,
        } += 1;
    }
    Ok(())
}

/// Creates an index database at `out` through a temporary file and a
/// rename, so `out` is either absent, the previous file, or complete.
pub(crate) fn write_database(
    out: &Path,
    fill: impl FnOnce(&rusqlite::Transaction<'_>) -> rusqlite::Result<()>,
) -> Result<(), PackageError> {
    let mut temporary = out.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    let _ = std::fs::remove_file(&temporary);
    {
        let mut db = Connection::open(&temporary)?;
        db.execute_batch("PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;")?;
        db.execute_batch(ENTRY_TABLES)?;
        let tx = db.transaction()?;
        fill(&tx)?;
        tx.commit()?;
        db.execute_batch("INSERT INTO entries_fts (entries_fts) VALUES ('optimize');")?;
    }
    std::fs::rename(&temporary, out)?;
    Ok(())
}

fn write_index(
    entries: &[Entry],
    towns: &TownGrid,
    package_sha256: &str,
    out: &Path,
    stats: &mut IndexStats,
) -> Result<(), PackageError> {
    write_database(out, |tx| {
        insert_entries(tx, entries, towns, stats)?;
        let mut meta = tx.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        meta.execute(params!["schema", SEARCH_SCHEMA.to_string()])?;
        meta.execute(params!["package_sha256", package_sha256])?;
        meta.execute(params!["built_at", crate::time::utc_now()])?;
        meta.execute(params![
            "stats",
            serde_json::to_string(&*stats).expect("stats serialize")
        ])?;
        Ok(())
    })
}

/// Word pairs treated as equivalent in names and queries (full, short).
const ABBREVIATIONS: [(&str, &str); 26] = [
    ("street", "st"),
    ("avenue", "ave"),
    ("road", "rd"),
    ("drive", "dr"),
    ("boulevard", "blvd"),
    ("lane", "ln"),
    ("court", "ct"),
    ("place", "pl"),
    ("parkway", "pkwy"),
    ("highway", "hwy"),
    ("expressway", "expy"),
    ("terrace", "ter"),
    ("circle", "cir"),
    ("trail", "trl"),
    ("square", "sq"),
    ("north", "n"),
    ("south", "s"),
    ("east", "e"),
    ("west", "w"),
    ("saint", "st"),
    ("mount", "mt"),
    ("fort", "ft"),
    ("interstate", "i"),
    ("route", "rte"),
    ("center", "ctr"),
    ("hospital", "hosp"),
];

/// Lower-case alphanumeric tokens, the way the FTS tokenizer splits text.
pub(crate) fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn alternatives(token: &str) -> Vec<&'static str> {
    ABBREVIATIONS
        .iter()
        .filter_map(|(full, short)| {
            if token == *full {
                Some(*short)
            } else if token == *short {
                Some(*full)
            } else {
                None
            }
        })
        .collect()
}

/// Index text: every token plus its abbreviation partner.
pub(crate) fn expand(text: &str) -> String {
    let mut out = Vec::new();
    for token in tokens(text) {
        let alts = alternatives(&token);
        out.push(token);
        out.extend(alts.into_iter().map(str::to_owned));
    }
    out.join(" ")
}

/// Comparison form: tokens with full words replaced by their short forms.
pub(crate) fn canonical(text: &str) -> String {
    tokens(text)
        .into_iter()
        .map(|t| {
            ABBREVIATIONS
                .iter()
                .find(|(full, _)| *full == t)
                .map_or(t.clone(), |(_, short)| (*short).to_owned())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drops a leading and trailing compass word from a canonical name
/// ("w main st" -> "main st", "lake shore dr n" -> "lake shore dr").
pub(crate) fn without_compass_words(canonical_name: &str) -> &str {
    const COMPASS: [&str; 8] = ["n", "s", "e", "w", "ne", "nw", "se", "sw"];
    let mut name = canonical_name;
    if let Some((first, rest)) = name.split_once(' ')
        && COMPASS.contains(&first)
    {
        name = rest;
    }
    if let Some((rest, last)) = name.rsplit_once(' ')
        && COMPASS.contains(&last)
    {
        name = rest;
    }
    name
}

pub struct SearchIndex {
    db: Connection,
    /// Stations from the raw OpenStreetMap extract, when installed.
    extras: Option<Connection>,
}

/// Sort key: match class, not a settlement, tier, not exact, then distance or
/// negative population.
type Order = (u8, bool, u8, u8, f64);

/// Opens the active package's search index, building it first when it is
/// missing or belongs to a different package (for example, a package
/// installed before search existed).
pub fn open_or_build(
    active: &crate::store::ActivePackage,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<SearchIndex, PackageError> {
    let path = active.search_index_path();
    if let Ok(index) = SearchIndex::open(&path, &active.record.sha256) {
        return Ok(index);
    }
    let package = TilePackage::open(&active.path)?;
    build_index(&package, &active.record.sha256, &path, cancelled, progress)?;
    SearchIndex::open(&path, &active.record.sha256)
}

impl SearchIndex {
    /// Opens an index and checks it belongs to the package with `package_sha256`.
    pub fn open(path: &Path, package_sha256: &str) -> Result<Self, PackageError> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let get = |key: &str| -> Result<String, PackageError> {
            db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
                .map_err(|_| PackageError::Invalid(format!("search index has no {key}")))
        };
        if get("schema")? != SEARCH_SCHEMA.to_string() {
            return Err(PackageError::Invalid(
                "search index schema is outdated".into(),
            ));
        }
        if get("package_sha256")? != package_sha256 {
            return Err(PackageError::Invalid(
                "search index belongs to another package".into(),
            ));
        }
        Ok(Self { db, extras: None })
    }

    pub fn stats(&self) -> Result<IndexStats, PackageError> {
        let text: String =
            self.db
                .query_row("SELECT value FROM meta WHERE key = 'stats'", [], |r| {
                    r.get(0)
                })?;
        serde_json::from_str(&text).map_err(|e| PackageError::Invalid(e.to_string()))
    }

    /// Adds an extras index (see `extras`) so its stations are searched too.
    pub fn attach_extras(&mut self, path: &Path) -> Result<(), PackageError> {
        self.extras = Some(crate::extras::open_entries(path)?);
        Ok(())
    }

    /// Finds up to `limit` matches for `query` in this index and its extras;
    /// see [`search_all`].
    pub fn search(
        &self,
        query: &str,
        near: Option<Coordinates>,
        limit: usize,
    ) -> Result<Vec<SearchResult>, PackageError> {
        search_all(&[self], query, near, limit)
    }

    /// The nearest city, town, village or hamlet within ~40 km.
    pub fn nearest_place(&self, at: Coordinates) -> Result<Option<SearchResult>, PackageError> {
        let (lat, lon) = (at.latitude(), at.longitude());
        let mut statement = self.db.prepare_cached(
            "SELECT name, category, lat, lon FROM entries
             WHERE kind = 'place' AND tier <= 3
               AND lat BETWEEN ?1 - 0.4 AND ?1 + 0.4 AND lon BETWEEN ?2 - 0.5 AND ?2 + 0.5",
        )?;
        let mut best: Option<SearchResult> = None;
        let mut rows = statement.query(params![lat, lon])?;
        while let Some(row) = rows.next()? {
            let (plat, plon): (f64, f64) = (row.get(2)?, row.get(3)?);
            let d = distance_km(lat, lon, plat, plon);
            if d <= 40.0
                && best
                    .as_ref()
                    .is_none_or(|b| b.distance_km.is_some_and(|bd| d < bd))
            {
                best = Some(SearchResult {
                    name: row.get(0)?,
                    kind: EntryKind::Place,
                    category: row.get(1)?,
                    near: None,
                    latitude: plat,
                    longitude: plon,
                    reference: None,
                    distance_km: Some(d),
                    detail: None,
                });
            }
        }
        Ok(best)
    }
}

/// Finds up to `limit` matches for `query` across several regions' indexes,
/// ranked together. Every query word must match a word of the name, route
/// number, category or nearest town; the last word may be a prefix. A query
/// starting with a house number lists addresses first.
///
/// Strong matches (name equal to or starting with the query, a route
/// number, or a category such as "hospital") come before matches on other
/// words, which come before matches through the nearest town. Within each,
/// cities, towns and villages come first, by exact name and then size; the
/// rest, with `near`, by distance, otherwise by place size and population.
///
/// Things mapped in two neighbouring regions' data appear once.
pub fn search_all(
    indexes: &[&SearchIndex],
    query: &str,
    near: Option<Coordinates>,
    limit: usize,
) -> Result<Vec<SearchResult>, PackageError> {
    let words: Vec<String> = tokens(query).into_iter().take(8).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for db in indexes.iter().filter_map(|i| i.extras.as_ref()) {
        if let Some(f) = crate::addresses::search_ranked(db, query, near)? {
            found.push(f);
        }
    }
    let addresses = crate::addresses::Found::join(found, limit);
    let expression = match_expression(&words);
    let query_canonical = canonical(query);
    let mut candidates = Vec::new();
    for index in indexes {
        for db in std::iter::once(&index.db).chain(index.extras.as_ref()) {
            collect_candidates(db, &expression, &query_canonical, near, &mut candidates)?;
        }
    }
    candidates.sort_by(|(a, _), (b, _)| {
        (a.0, a.1, a.2, a.3)
            .cmp(&(b.0, b.1, b.2, b.3))
            .then(a.4.total_cmp(&b.4))
    });
    // Streets are one entry per name and town; points of interest, stops
    // and places with the same name are kept apart unless they are close
    // together (the same thing mapped twice, or in two regions' data).
    let mut seen = std::collections::HashSet::new();
    Ok(addresses
        .into_iter()
        .chain(candidates.into_iter().map(|(_, r)| r))
        .filter(|r| {
            let spot = match r.kind {
                EntryKind::Poi | EntryKind::Transit => Some(cell(r.longitude, r.latitude, 200.0)),
                EntryKind::Place | EntryKind::Address => Some(cell(r.longitude, r.latitude, 20.0)),
                EntryKind::Street | EntryKind::Water => None,
            };
            seen.insert((
                r.name.clone(),
                r.kind,
                r.near.clone(),
                r.category.clone(),
                spot,
            ))
        })
        .take(limit)
        .collect())
}

/// FTS5 expression: every word must match (with its abbreviation partner);
/// the last word of two or more characters may be a prefix.
pub(crate) fn match_expression(words: &[String]) -> String {
    let last = words.len().saturating_sub(1);
    words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let prefix = i == last && word.chars().count() >= 2;
            let mut forms = vec![word.as_str()];
            forms.extend(alternatives(word));
            let terms: Vec<String> = forms
                .iter()
                .map(|f| {
                    format!(
                        "\"{}\"{}",
                        f.replace('"', ""),
                        if prefix { "*" } else { "" }
                    )
                })
                .collect();
            format!("({})", terms.join(" OR "))
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Runs the full-text match on one database and ranks each row.
fn collect_candidates(
    db: &Connection,
    expression: &str,
    query_canonical: &str,
    near: Option<Coordinates>,
    out: &mut Vec<(Order, SearchResult)>,
) -> Result<(), PackageError> {
    let (lat0, lon0) = near.map_or((0.0, 0.0), |c| (c.latitude(), c.longitude()));
    let scale = lat0.to_radians().cos().powi(2);
    let mut statement = db.prepare_cached(
        "SELECT e.name, e.kind, e.category, e.near, e.lat, e.lon, e.tier, e.population,
                e.reference, e.match_name, e.match_ref, e.detail
         FROM entries_fts f JOIN entries e ON e.id = f.rowid
         WHERE entries_fts MATCH ?1
         ORDER BY CASE WHEN ?2 THEN (e.lat - ?3) * (e.lat - ?3) + (e.lon - ?4) * (e.lon - ?4) * ?5
                       ELSE e.tier * 1e12 - e.population END
         LIMIT 5000",
    )?;
    let query_words: Vec<&str> = query_canonical.split(' ').collect();
    let words_in = |text: &str| {
        let text_words: Vec<&str> = text.split(' ').collect();
        query_words.iter().enumerate().all(|(i, q)| {
            text_words
                .iter()
                .any(|w| w == q || (i == query_words.len() - 1 && w.starts_with(q)))
        })
    };
    let mut rows = statement.query(params![expression, near.is_some(), lat0, lon0, scale])?;
    while let Some(row) = rows.next()? {
        let kind = EntryKind::from_code(&row.get::<_, String>(1)?).unwrap_or(EntryKind::Poi);
        let category: String = row.get(2)?;
        let (lat, lon): (f64, f64) = (row.get(4)?, row.get(5)?);
        let tier: u8 = row.get(6)?;
        let population: i64 = row.get(7)?;
        let (match_name, match_ref): (String, String) = (row.get(9)?, row.get(10)?);
        // "West Main Street" is a strong match for "main street".
        let core_name = without_compass_words(&match_name);
        let exact = match_name == query_canonical || core_name == query_canonical;
        // "gas station" is a strong match for the category "fuel".
        let category_words = canonical(&category_words(&category));
        let strong = exact
            || match_name.starts_with(query_canonical)
            || core_name.starts_with(query_canonical)
            || (!match_ref.is_empty() && match_ref.starts_with(query_canonical))
            || (!category_words.is_empty()
                && format!(" {category_words}").contains(&format!(" {query_canonical}")));
        let class = if strong {
            0
        } else if words_in(&format!("{match_name} {match_ref} {category_words}")) {
            1
        } else {
            2
        };
        let settlement = kind == EntryKind::Place && tier <= 2;
        let distance = near.map(|c| distance_km(c.latitude(), c.longitude(), lat, lon));
        // Settlements: an exact name, then cities before towns before
        // villages, so "carb" puts Carbondale above nearer Carbon Hill.
        let order = match distance {
            Some(d) if settlement => (class, false, u8::from(!exact), tier, d),
            Some(d) => (class, true, 0, 0, d),
            None => (
                class,
                !settlement,
                tier,
                u8::from(!exact),
                -population as f64,
            ),
        };
        out.push((
            order,
            SearchResult {
                name: row.get(0)?,
                kind,
                category,
                near: row.get(3)?,
                latitude: lat,
                longitude: lon,
                reference: row.get(8)?,
                distance_km: distance,
                detail: row.get(11)?,
            },
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "search_tests.rs"]
pub(crate) mod tests;
