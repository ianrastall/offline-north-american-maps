//! Fuel and charging stations from the region's raw OpenStreetMap extract.
//!
//! Geofabrik's Shortbread tiles leave out `amenity=fuel` and
//! `amenity=charging_station`. They are read from the region's `.osm.pbf`
//! (stations mapped as points, areas or multipolygons) into a small extras
//! database that search and the map use beside the package. The raw file is
//! deleted once the stations are extracted.
//!
//! ```text
//! <root>/<region>/extras/active.json
//! <root>/<region>/extras/versions/<v>/extras.sqlite
//! <root>/<region>/extras/versions/<v>/record.json
//! <root>/<region>/extras/staging/...        download in progress, as for packages
//! ```
//!
//! Extras are independent of the map package: either can be updated, and a
//! missing or failed extras update never affects the map.
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};

use crate::download::{
    Progress, Transport, UpdateError, UpdateOptions, check_size, download_to_staging, is_older,
    network,
};
use crate::mbtiles::{Bounds, PackageError};
use crate::osm_pbf::{Element, Kinds, OsmHeader, PbfError, Tags, scan, tag};
use crate::package::{Region, SourceInfo, sha256_file};
use crate::search::{
    Entry, EntryKind, IndexStats, SEARCH_SCHEMA, TownGrid, insert_entries, write_database,
};
use crate::store::{PackageStore, RECORD_FILE, is_version_name, write_atomic};

pub const EXTRAS_FILE: &str = "extras.sqlite";
pub const EXTRAS_SCHEMA: u32 = 1;
const STATION_KINDS: [&str; 2] = ["fuel", "charging_station"];
const TOWN_KINDS: [&str; 5] = ["city", "town", "village", "hamlet", "suburb"];
/// Fuels worth showing, with display names. Octane grades are left out.
const FUELS: [(&str, &str); 7] = [
    ("diesel", "diesel"),
    ("HGV_diesel", "truck diesel"),
    ("biodiesel", "biodiesel"),
    ("e85", "E85"),
    ("lpg", "LPG"),
    ("cng", "CNG"),
    ("adblue", "DEF"),
];
/// Two stations closer than this, of the same kind and name, are one station
/// mapped twice (typically the forecourt area and a point inside it).
const DUPLICATE_KM: f64 = 0.15;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Station {
    /// `node/1`, `way/2` or `relation/3`.
    pub osm_id: String,
    /// `fuel` or `charging_station`.
    pub category: String,
    /// `name`, else `brand`, else `operator`; may be empty.
    pub name: String,
    pub brand: Option<String>,
    pub operator: Option<String>,
    pub opening_hours: Option<String>,
    pub fuels: Vec<String>,
    pub address: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
}

impl Station {
    fn from_tags(osm_id: String, tags: &Tags<'_>) -> Option<Self> {
        let category = tag(tags, "amenity").filter(|a| STATION_KINDS.contains(a))?;
        if matches!(tag(tags, "access"), Some("private" | "no")) {
            return None;
        }
        let text = |key: &str| {
            tag(tags, key)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        };
        let brand = text("brand");
        let operator = text("operator");
        let name = text("name")
            .or_else(|| brand.clone())
            .or_else(|| operator.clone())
            .unwrap_or_default();
        let fuels = FUELS
            .iter()
            .filter(|(key, _)| tag(tags, &format!("fuel:{key}")) == Some("yes"))
            .map(|(_, label)| (*label).to_owned())
            .collect();
        let street = match (text("addr:housenumber"), text("addr:street")) {
            (Some(number), Some(street)) => Some(format!("{number} {street}")),
            (None, Some(street)) => Some(street),
            _ => None,
        };
        let address = street.map(|a| match text("addr:city") {
            Some(city) => format!("{a}, {city}"),
            None => a,
        });
        Some(Self {
            osm_id,
            category: category.to_owned(),
            name,
            brand,
            operator,
            opening_hours: text("opening_hours"),
            fuels,
            address,
            latitude: 0.0,
            longitude: 0.0,
        })
    }

    /// The name shown in search and on the map.
    pub fn display_name(&self) -> String {
        match (self.name.is_empty(), self.category.as_str()) {
            (false, _) => self.name.clone(),
            (true, "fuel") => "Fuel station".into(),
            (true, _) => "Charging station".into(),
        }
    }

    /// Brand, hours, fuels and address, for example
    /// `Shell · open 24/7 · diesel, E85 · 101 N Main St, Clinton`.
    pub fn detail(&self) -> Option<String> {
        let mut parts = Vec::new();
        let differs = |v: &Option<String>| v.as_ref().filter(|v| **v != self.name).cloned();
        if let Some(brand) = differs(&self.brand).or_else(|| differs(&self.operator)) {
            parts.push(brand);
        }
        match self.opening_hours.as_deref() {
            Some("24/7") => parts.push("open 24/7".into()),
            Some(hours) => parts.push(format!("hours {hours}")),
            None => {}
        }
        if !self.fuels.is_empty() {
            parts.push(self.fuels.join(", "));
        }
        parts.extend(self.address.clone());
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    fn richness(&self) -> usize {
        [
            !self.name.is_empty(),
            self.brand.is_some(),
            self.operator.is_some(),
            self.opening_hours.is_some(),
            !self.fuels.is_empty(),
            self.address.is_some(),
        ]
        .into_iter()
        .filter(|v| *v)
        .count()
    }

    fn same_station(&self, other: &Station) -> bool {
        self.category == other.category
            && (self.name.is_empty()
                || other.name.is_empty()
                || self.name.eq_ignore_ascii_case(&other.name)
                || (self.brand.is_some() && self.brand == other.brand))
            && crate::search::distance_km(
                self.latitude,
                self.longitude,
                other.latitude,
                other.longitude,
            ) < DUPLICATE_KM
    }

    /// Fills fields this copy lacks from another copy of the same station.
    fn absorb(&mut self, other: Station) {
        if self.name.is_empty() {
            self.name = other.name;
        }
        self.brand = self.brand.take().or(other.brand);
        self.operator = self.operator.take().or(other.operator);
        self.opening_hours = self.opening_hours.take().or(other.opening_hours);
        self.address = self.address.take().or(other.address);
        if self.fuels.is_empty() {
            self.fuels = other.fuels;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractStats {
    pub fuel: u64,
    pub charging: u64,
    /// Copies of a station merged into another (area plus point, for example).
    pub merged: u64,
    /// Areas whose nodes are missing from the extract.
    pub unlocated: u64,
    pub towns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Extract {
    pub header: OsmHeader,
    pub stations: Vec<Station>,
    /// Name, longitude and latitude of each city, town, village, hamlet and suburb.
    pub towns: Vec<(String, f64, f64)>,
    pub stats: ExtractStats,
}

#[derive(Default)]
struct FirstPass {
    stations: Vec<Station>,
    areas: Vec<(Station, Vec<i64>)>,
    multipolygons: Vec<(Station, Vec<i64>)>,
    towns: Vec<(String, f64, f64)>,
}

#[derive(Default)]
struct Resolved {
    nodes: Vec<(i64, f64, f64)>,
    ways: Vec<(i64, Vec<i64>)>,
}

const ALL: Kinds = Kinds {
    nodes: true,
    ways: true,
    relations: true,
};

/// Reads stations and towns from `path`. Areas need a second pass to find
/// their nodes' positions, and multipolygons a third (their member ways come
/// after the nodes in the file). `progress` receives (step, bytes read, file
/// length) with step 1 to 3.
pub fn extract(
    path: &Path,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u8, u64, u64),
) -> Result<Extract, PbfError> {
    let mut first = FirstPass::default();
    let header = scan(
        path,
        cancelled,
        &mut |read, total| progress(1, read, total),
        &|block| {
            let mut found = FirstPass::default();
            block.for_each(ALL, &mut |element, tags| {
                if tags.is_empty() {
                    return;
                }
                match element {
                    Element::Node { id, lat, lon } => {
                        if let (Some(place), Some(name)) = (tag(tags, "place"), tag(tags, "name"))
                            && TOWN_KINDS.contains(&place)
                            && !name.trim().is_empty()
                        {
                            found.towns.push((name.trim().to_owned(), lon, lat));
                        }
                        if let Some(mut station) = Station::from_tags(format!("node/{id}"), tags) {
                            station.latitude = lat;
                            station.longitude = lon;
                            found.stations.push(station);
                        }
                    }
                    Element::Way { id, refs } => {
                        if let Some(station) = Station::from_tags(format!("way/{id}"), tags) {
                            found.areas.push((station, refs.to_vec()));
                        }
                    }
                    Element::Relation { id, members } => {
                        if tag(tags, "type") == Some("multipolygon")
                            && let Some(station) =
                                Station::from_tags(format!("relation/{id}"), tags)
                        {
                            let outer = members
                                .iter()
                                .filter(|m| {
                                    m.kind == crate::osm_pbf::MemberKind::Way
                                        && matches!(m.role, "outer" | "")
                                })
                                .map(|m| m.id)
                                .collect();
                            found.multipolygons.push((station, outer));
                        }
                    }
                }
            })?;
            Ok(found)
        },
        &mut |found| {
            first.stations.extend(found.stations);
            first.areas.extend(found.areas);
            first.multipolygons.extend(found.multipolygons);
            first.towns.extend(found.towns);
        },
    )?;

    // Second pass: node positions for areas, node lists for multipolygon ways.
    let mut positions: HashMap<i64, (f64, f64)> = HashMap::new();
    let mut member_ways: HashMap<i64, Vec<i64>> = HashMap::new();
    let wanted_nodes: HashSet<i64> = first
        .areas
        .iter()
        .flat_map(|(_, r)| r.iter().copied())
        .collect();
    let wanted_ways: HashSet<i64> = first
        .multipolygons
        .iter()
        .flat_map(|(_, w)| w.iter().copied())
        .collect();
    if !wanted_nodes.is_empty() || !wanted_ways.is_empty() {
        resolve(
            path,
            2,
            &wanted_nodes,
            &wanted_ways,
            cancelled,
            progress,
            &mut positions,
            &mut member_ways,
        )?;
    }
    // Third pass: node positions for multipolygon ways.
    let missing: HashSet<i64> = member_ways
        .values()
        .flatten()
        .copied()
        .filter(|n| !positions.contains_key(n))
        .collect();
    if !missing.is_empty() {
        resolve(
            path,
            3,
            &missing,
            &HashSet::new(),
            cancelled,
            progress,
            &mut positions,
            &mut member_ways,
        )?;
    }

    let mut stats = ExtractStats::default();
    let mut stations = first.stations;
    for (mut station, refs) in first.areas {
        match centre(&refs, &positions) {
            Some((lat, lon)) => {
                (station.latitude, station.longitude) = (lat, lon);
                stations.push(station);
            }
            None => stats.unlocated += 1,
        }
    }
    for (mut station, ways) in first.multipolygons {
        let refs: Vec<i64> = ways
            .iter()
            .filter_map(|w| member_ways.get(w))
            .flatten()
            .copied()
            .collect();
        match centre(&refs, &positions) {
            Some((lat, lon)) => {
                (station.latitude, station.longitude) = (lat, lon);
                stations.push(station);
            }
            None => stats.unlocated += 1,
        }
    }
    let (stations, merged) = merge_duplicates(stations);
    stats.merged = merged;
    stats.fuel = stations.iter().filter(|s| s.category == "fuel").count() as u64;
    stats.charging = stations.len() as u64 - stats.fuel;
    stats.towns = first.towns.len() as u64;
    Ok(Extract {
        header,
        stations,
        towns: first.towns,
        stats,
    })
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    path: &Path,
    step: u8,
    nodes: &HashSet<i64>,
    ways: &HashSet<i64>,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(u8, u64, u64),
    positions: &mut HashMap<i64, (f64, f64)>,
    member_ways: &mut HashMap<i64, Vec<i64>>,
) -> Result<(), PbfError> {
    let kinds = Kinds {
        nodes: !nodes.is_empty(),
        ways: !ways.is_empty(),
        relations: false,
    };
    scan(
        path,
        cancelled,
        &mut |read, total| progress(step, read, total),
        &|block| {
            let mut found = Resolved::default();
            block.for_each(kinds, &mut |element, _| match element {
                Element::Node { id, lat, lon } if nodes.contains(&id) => {
                    found.nodes.push((id, lat, lon))
                }
                Element::Way { id, refs } if ways.contains(&id) => {
                    found.ways.push((id, refs.to_vec()))
                }
                _ => {}
            })?;
            Ok(found)
        },
        &mut |found| {
            positions.extend(
                found
                    .nodes
                    .into_iter()
                    .map(|(id, lat, lon)| (id, (lat, lon))),
            );
            member_ways.extend(found.ways);
        },
    )?;
    Ok(())
}

/// Mean position of an area's distinct nodes, ignoring nodes outside the
/// extract. Station forecourts are small, so the mean lies on the site.
fn centre(refs: &[i64], positions: &HashMap<i64, (f64, f64)>) -> Option<(f64, f64)> {
    let mut seen = HashSet::new();
    let (mut lat, mut lon, mut n) = (0.0, 0.0, 0.0);
    for (la, lo) in refs
        .iter()
        .filter(|r| seen.insert(**r))
        .filter_map(|r| positions.get(r))
    {
        lat += la;
        lon += lo;
        n += 1.0;
    }
    (n > 0.0).then(|| (lat / n, lon / n))
}

/// Keeps the most complete copy of each station and fills its gaps from the
/// others. Returns the stations and how many copies were merged.
fn merge_duplicates(mut stations: Vec<Station>) -> (Vec<Station>, u64) {
    stations.sort_by(|a, b| {
        b.richness()
            .cmp(&a.richness())
            .then_with(|| a.osm_id.cmp(&b.osm_id))
    });
    let cell = |s: &Station| {
        (
            (s.longitude * 500.0).floor() as i64,
            (s.latitude * 500.0).floor() as i64,
        )
    };
    let mut kept: Vec<Station> = Vec::new();
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    let mut merged = 0;
    for station in stations {
        let (cx, cy) = cell(&station);
        let duplicate_of = (-1..=1)
            .flat_map(|dx| (-1..=1).map(move |dy| (cx + dx, cy + dy)))
            .filter_map(|c| grid.get(&c))
            .flatten()
            .copied()
            .find(|&i| kept[i].same_station(&station));
        match duplicate_of {
            Some(i) => {
                kept[i].absorb(station);
                merged += 1;
            }
            None => {
                grid.entry((cx, cy)).or_default().push(kept.len());
                kept.push(station);
            }
        }
    }
    kept.sort_by(|a, b| a.osm_id.cmp(&b.osm_id));
    (kept, merged)
}

/// Writes the extras database: the stations and towns themselves (so search
/// entries can be rebuilt without the raw file) and their search entries.
pub fn write_extras(
    extract: &Extract,
    source_sha256: &str,
    out: &Path,
) -> Result<IndexStats, PackageError> {
    let towns = TownGrid::from_points(extract.towns.iter().cloned());
    // The tier search uses for emergency services.
    let tier = 2;
    let entries: Vec<Entry> = extract
        .stations
        .iter()
        .map(|s| {
            let name = s.display_name();
            Entry {
                alt_name: s
                    .brand
                    .clone()
                    .or_else(|| s.operator.clone())
                    .filter(|b| *b != name),
                name,
                reference: None,
                kind: EntryKind::Poi,
                category: s.category.clone(),
                lon: s.longitude,
                lat: s.latitude,
                tier,
                population: 0,
                detail: s.detail(),
            }
        })
        .collect();
    let mut stats = IndexStats::default();
    write_database(out, |tx| {
        insert_entries(tx, &entries, &towns, &mut stats)?;
        tx.execute_batch(
            "CREATE TABLE stations (json TEXT NOT NULL);
             CREATE TABLE towns (name TEXT NOT NULL, lon REAL NOT NULL, lat REAL NOT NULL);",
        )?;
        let mut station = tx.prepare("INSERT INTO stations (json) VALUES (?1)")?;
        for s in &extract.stations {
            station.execute([serde_json::to_string(s).expect("station serializes")])?;
        }
        let mut town = tx.prepare("INSERT INTO towns (name, lon, lat) VALUES (?1, ?2, ?3)")?;
        for (name, lon, lat) in &extract.towns {
            town.execute(params![name, lon, lat])?;
        }
        let mut meta = tx.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        meta.execute(params!["extras_schema", EXTRAS_SCHEMA.to_string()])?;
        meta.execute(params!["search_schema", SEARCH_SCHEMA.to_string()])?;
        meta.execute(params!["source_sha256", source_sha256])?;
        meta.execute(params!["built_at", crate::time::utc_now()])?;
        meta.execute(params![
            "stats",
            serde_json::to_string(&extract.stats).expect("stats serialize")
        ])?;
        Ok(())
    })?;
    Ok(stats)
}

fn meta(db: &Connection, key: &str) -> Result<String, PackageError> {
    db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .map_err(|_| PackageError::Invalid(format!("extras file has no {key}")))
}

/// Rewrites an extras file whose search entries predate the current search
/// format, from the stations and towns it stores.
fn rebuild(path: &Path) -> Result<(), PackageError> {
    let (extract, sha) = {
        let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let stations = db
            .prepare("SELECT json FROM stations")?
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|json| {
                serde_json::from_str(&json?).map_err(|e| PackageError::Invalid(e.to_string()))
            })
            .collect::<Result<Vec<Station>, _>>()?;
        let towns = db
            .prepare("SELECT name, lon, lat FROM towns")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let stats = serde_json::from_str(&meta(&db, "stats")?)
            .map_err(|e| PackageError::Invalid(e.to_string()))?;
        (
            Extract {
                header: OsmHeader::default(),
                stations,
                towns,
                stats,
            },
            meta(&db, "source_sha256")?,
        )
    };
    write_extras(&extract, &sha, path).map(|_| ())
}

/// Opens an extras file's search entries, rebuilding them first if the search
/// format has changed since the file was written.
pub fn open_entries(path: &Path) -> Result<Connection, PackageError> {
    let open = || -> Result<Connection, PackageError> {
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        if meta(&db, "extras_schema")? != EXTRAS_SCHEMA.to_string() {
            return Err(PackageError::Invalid(
                "extras file format is outdated".into(),
            ));
        }
        Ok(db)
    };
    let db = open()?;
    if meta(&db, "search_schema")? == SEARCH_SCHEMA.to_string() {
        return Ok(db);
    }
    drop(db);
    rebuild(path)?;
    open()
}

/// Stations as a GeoJSON feature collection for the map, with `name`,
/// `category` and `detail` properties.
pub fn stations_geojson(path: &Path) -> Result<String, PackageError> {
    let db = open_entries(path)?;
    let mut statement = db.prepare("SELECT name, category, detail, lon, lat FROM entries")?;
    let features = statement
        .query_map([], |r| {
            Ok(serde_json::json!({
                "type": "Feature",
                "geometry": {
                    "type": "Point",
                    "coordinates": [
                        (r.get::<_, f64>(3)? * 1e6).round() / 1e6,
                        (r.get::<_, f64>(4)? * 1e6).round() / 1e6,
                    ],
                },
                "properties": {
                    "name": r.get::<_, String>(0)?,
                    "category": r.get::<_, String>(1)?,
                    "detail": r.get::<_, Option<String>>(2)?,
                },
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({ "type": "FeatureCollection", "features": features }).to_string())
}

/// Persisted beside each extras version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtrasRecord {
    pub schema: u32,
    pub region_id: String,
    pub source: SourceInfo,
    /// Size and SHA-256 of the raw extract the stations were read from.
    pub byte_length: u64,
    pub sha256: String,
    /// OSM data time from the extract's header, when recorded (RFC 3339).
    pub data_timestamp: Option<String>,
    pub downloaded_at: String,
    pub built_at: String,
    pub stats: ExtractStats,
    pub attribution: String,
    pub license: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActiveExtras {
    pub version: String,
    /// The extras database.
    pub path: PathBuf,
    pub record: ExtrasRecord,
}

/// The store id under which a region's extras are kept (`us-il/extras`).
pub fn extras_id(region_id: &str) -> String {
    format!("{region_id}/extras")
}

#[derive(Serialize, Deserialize)]
struct Pointer {
    schema: u32,
    version: String,
}

/// The region's active extras, if any.
pub fn active_extras(
    store: &PackageStore,
    region_id: &str,
) -> Result<Option<ActiveExtras>, PackageError> {
    let id = extras_id(region_id);
    let text = match fs::read_to_string(store.region_dir(&id).join("active.json")) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let pointer: Pointer = serde_json::from_str(&text)
        .map_err(|e| PackageError::Invalid(format!("extras pointer is unreadable: {e}")))?;
    if pointer.schema != 1 || !is_version_name(&pointer.version) {
        return Err(PackageError::Invalid("extras pointer is invalid".into()));
    }
    let dir = store.version_dir(&id, &pointer.version);
    let record: ExtrasRecord = serde_json::from_str(&fs::read_to_string(dir.join(RECORD_FILE))?)
        .map_err(|e| PackageError::Invalid(format!("extras record is unreadable: {e}")))?;
    let path = dir.join(EXTRAS_FILE);
    if record.region_id != region_id || !path.is_file() {
        return Err(PackageError::Invalid(
            "extras do not match their record".into(),
        ));
    }
    Ok(Some(ActiveExtras {
        version: pointer.version,
        path,
        record,
    }))
}

/// Installs a built extras file as a new version and makes it active.
fn install(
    store: &PackageStore,
    region_id: &str,
    version: &str,
    built: &Path,
    record: &ExtrasRecord,
) -> io::Result<()> {
    let id = extras_id(region_id);
    let dir = store.version_dir(&id, version);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    fs::rename(built, dir.join(EXTRAS_FILE))?;
    let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    write_atomic(&dir.join(RECORD_FILE), &json)?;
    let pointer = serde_json::to_vec(&Pointer {
        schema: 1,
        version: version.to_owned(),
    })
    .map_err(io::Error::other)?;
    write_atomic(&store.region_dir(&id).join("active.json"), &pointer)
}

/// Removes inactive extras versions; returns how many could not be removed yet.
fn prune(store: &PackageStore, region_id: &str, active: &str) -> usize {
    let id = extras_id(region_id);
    let Ok(entries) = fs::read_dir(store.region_dir(&id).join("versions")) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != active)
        .filter(|e| fs::remove_dir_all(e.path()).is_err())
        .count()
}

#[derive(Debug)]
pub enum ExtrasOutcome {
    UpToDate(ActiveExtras),
    Activated {
        extras: ActiveExtras,
        replaced: Option<String>,
        prune_pending: usize,
    },
}

fn pbf_error(error: PbfError) -> UpdateError {
    match error {
        PbfError::Io(e) if e.kind() == io::ErrorKind::Interrupted => UpdateError::Cancelled,
        PbfError::Io(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            UpdateError::Invalid(PackageError::Invalid(e.to_string()))
        }
        PbfError::Io(e) => UpdateError::Storage(e),
        other => UpdateError::Invalid(PackageError::Invalid(other.to_string())),
    }
}

fn is_same_source(record: &ExtrasRecord, url: &str, remote: &crate::download::RemoteInfo) -> bool {
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

/// Brings the region's fuel and charging stations up to date with
/// `region.extras_url`. The raw extract is downloaded (resumably) into
/// staging, hashed, read, and deleted; only the extras database is kept.
/// Progress reports downloading, `Verifying` while hashing, and `Indexing`
/// with done/total in thousandths across the reading passes.
pub fn update_extras(
    store: &PackageStore,
    region: &Region,
    transport: &dyn Transport,
    options: &UpdateOptions,
    progress: &mut dyn FnMut(Progress),
    cancelled: &AtomicBool,
) -> Result<ExtrasOutcome, UpdateError> {
    let id = extras_id(region.id);
    let url = region.extras_url;
    let _lock = store.lock(&id).map_err(|e| match e.kind() {
        io::ErrorKind::WouldBlock => UpdateError::Busy,
        _ => UpdateError::Storage(e),
    })?;
    let active = active_extras(store, region.id).ok().flatten();

    progress(Progress::Checking);
    let remote = transport.probe(url).map_err(network)?;
    if let Some(active) = active.as_ref().filter(|a| {
        is_same_source(&a.record, url, &remote)
            || is_older(
                remote.last_modified.as_deref(),
                a.record.source.last_modified.as_deref(),
            )
    }) {
        return Ok(ExtrasOutcome::UpToDate(active.clone()));
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
    if active.as_ref().is_some_and(|a| {
        is_older(
            source.last_modified.as_deref(),
            a.record.source.last_modified.as_deref(),
        )
    }) {
        let _ = store.clear_staging(&id);
        return Ok(ExtrasOutcome::UpToDate(active.expect("checked")));
    }

    let mut report = |step: u8, read: u64, total: u64| {
        let done = u64::from(step - 1) * 1000 + read * 1000 / total.max(1);
        progress(Progress::Indexing { done, total: 3000 });
    };
    let extract = match extract(&part, cancelled, &mut report) {
        Ok(extract) => extract,
        Err(error) => {
            let error = pbf_error(error);
            if matches!(error, UpdateError::Invalid(_)) {
                let _ = store.clear_staging(&id);
            }
            return Err(error);
        }
    };
    if let Err(problem) = check_coverage(&extract, region) {
        let _ = store.clear_staging(&id);
        return Err(UpdateError::Invalid(PackageError::Invalid(problem)));
    }
    let built = store.staging_dir(&id).join(EXTRAS_FILE);
    write_extras(&extract, &sha256, &built).map_err(|e| match e {
        PackageError::Io(e) => UpdateError::Storage(e),
        other => UpdateError::Invalid(other),
    })?;

    progress(Progress::Activating);
    let built_at = (options.clock)();
    let record = ExtrasRecord {
        schema: 1,
        region_id: region.id.to_owned(),
        source,
        byte_length,
        sha256,
        data_timestamp: extract
            .header
            .replication_timestamp
            .map(crate::time::format_unix),
        downloaded_at,
        built_at: built_at.clone(),
        stats: extract.stats.clone(),
        attribution: crate::OSM_ATTRIBUTION.to_owned(),
        license: "ODbL-1.0".to_owned(),
    };
    let stamp: String = built_at
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let version = format!("{stamp}-{}", &record.sha256[..12]);
    install(store, region.id, &version, &built, &record).map_err(UpdateError::Storage)?;
    // The raw extract is not kept.
    let _ = store.clear_staging(&id);
    let extras = active_extras(store, region.id)
        .map_err(|e| UpdateError::Storage(io::Error::other(e.to_string())))?
        .ok_or_else(|| UpdateError::Storage(io::Error::other("Activated extras are missing")))?;
    let prune_pending = prune(store, region.id, &version);
    Ok(ExtrasOutcome::Activated {
        extras,
        replaced: active.map(|a| a.version),
        prune_pending,
    })
}

/// The extract must be for this region: its header box covers the region's
/// required points and lies inside the region's outer box.
fn check_coverage(extract: &Extract, region: &Region) -> Result<(), String> {
    let bbox: Bounds = extract
        .header
        .bbox
        .ok_or("The OpenStreetMap extract has no bounding box")?;
    let covers = region.required_points.iter().all(|(lon, lat)| {
        bbox.west <= *lon && *lon <= bbox.east && bbox.south <= *lat && *lat <= bbox.north
    });
    if !covers || !region.outer.contains(&bbox) {
        return Err(format!(
            "The OpenStreetMap extract does not match {}",
            region.name
        ));
    }
    if extract.towns.is_empty() {
        return Err("The OpenStreetMap extract contains no towns".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "extras_tests.rs"]
mod tests;
