//! The command layer behind the C ABI: the installed regions' map packages,
//! extras and search, and saved places. Safe to call from several threads at
//! once: tile reads, searches and saved-place changes take short locks, and an
//! update runs without holding any of them, so the map stays usable while a
//! new package downloads. Only one update, removal or search preparation runs
//! at a time.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use offline_maps_core::Coordinates;
use offline_maps_core::download::{
    HttpTransport, Progress, UpdateError, UpdateOptions, UpdateOutcome, update_region,
};
use offline_maps_core::extras::{
    ActiveExtras, ExtrasOutcome, active_extras, prune_extras, stations_geojson_all, update_extras,
};
use offline_maps_core::mbtiles::{Bounds, TileEncoding, TilePackage};
use offline_maps_core::merge::merge_tiles;
use offline_maps_core::mvt::tile_point_to_lon_lat;
use offline_maps_core::package::{ILLINOIS, Region};
use offline_maps_core::places::{PlacesError, SavedPlace, SavedPlaces};
use offline_maps_core::regions;
use offline_maps_core::search::{SearchIndex, open_or_build, search_all};
use offline_maps_core::store::{ActivePackage, PackageStore};
use offline_maps_core::terrain::{
    ActiveTerrain, TERRAIN_MAX_ZOOM, TERRAIN_URL, TerrainOutcome, TerrainTiles, active_terrain,
    prune_terrain, update_terrain,
};

use crate::messages as pb;
use pb::ErrorKind;

pub type Outcome<T> = Result<T, pb::Error>;

pub fn error(kind: ErrorKind, message: impl Into<String>) -> pb::Error {
    pb::Error {
        kind: kind as i32,
        message: message.into(),
    }
}

/// A lock that stays usable after a panic elsewhere; the ABI marks the core
/// unusable after a panic, so only cleanup reaches a poisoned lock.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct OpenMap {
    active: ActivePackage,
    tiles: TilePackage,
}

/// One installed region and what of it could be opened.
struct Slot {
    region: Region,
    /// The map's bounds, when it opened.
    bounds: Option<Bounds>,
    map: Mutex<Option<OpenMap>>,
    search: Mutex<Option<SearchIndex>>,
    extras: Option<ActiveExtras>,
    terrain: Option<ActiveTerrain>,
    terrain_tiles: Mutex<Option<TerrainTiles>>,
    problem: String,
}

enum Places {
    Ready(SavedPlaces),
    Unreadable(String),
}

pub struct Service {
    store: PackageStore,
    /// Test overrides of Illinois's download addresses.
    package_url: Option<&'static str>,
    extras_url: Option<&'static str>,
    terrain_url: String,
    /// Installed regions, in catalog order.
    slots: RwLock<Vec<Arc<Slot>>>,
    places: Mutex<Places>,
    progress: Mutex<pb::ProgressEvent>,
    cancel: AtomicBool,
    busy: AtomicBool,
}

/// Clears the busy flag and the progress state when an operation ends.
struct BusyGuard<'a>(&'a Service);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *lock(&self.0.progress) = pb::ProgressEvent::default();
        self.0.busy.store(false, Ordering::Release);
    }
}

/// `Sun, 08 Nov 2026 04:12:34 GMT` -> `2026-11-08`.
fn http_date_to_iso(value: Option<&str>) -> String {
    value
        .and_then(offline_maps_core::time::parse_http_date)
        .map(|(y, m, d, ..)| format!("{y:04}-{m:02}-{d:02}"))
        .unwrap_or_default()
}

fn covered(place: &SavedPlace, bounds: &[Bounds]) -> bool {
    bounds.iter().any(|b| place.is_covered_by(b))
}

fn place_message(place: &SavedPlace, bounds: &[Bounds]) -> pb::Place {
    pb::Place {
        id: place.id,
        name: place.name.clone(),
        note: place.note.clone(),
        latitude: place.latitude,
        longitude: place.longitude,
        covered: covered(place, bounds),
        created_at: place.created_at.clone(),
        updated_at: place.updated_at.clone(),
    }
}

fn places_error(e: PlacesError) -> pb::Error {
    let kind = match e {
        PlacesError::Invalid(_) | PlacesError::Full => ErrorKind::InvalidArgument,
        PlacesError::NotFound(_) => ErrorKind::NotFound,
        PlacesError::Unreadable(_) => ErrorKind::InvalidData,
        PlacesError::Io(_) => ErrorKind::Storage,
    };
    error(kind, e.to_string())
}

fn update_error(e: UpdateError) -> pb::Error {
    let kind = match e {
        UpdateError::Busy => ErrorKind::Busy,
        UpdateError::Cancelled => ErrorKind::Cancelled,
        UpdateError::Network(_) => ErrorKind::Network,
        UpdateError::Storage(_)
        | UpdateError::TooLarge { .. }
        | UpdateError::InsufficientSpace { .. } => ErrorKind::Storage,
        UpdateError::Invalid(_) => ErrorKind::InvalidData,
    };
    error(kind, e.to_string())
}

fn position(p: Option<&pb::Position>) -> Outcome<Option<Coordinates>> {
    p.map(|p| {
        Coordinates::new(p.latitude, p.longitude)
            .ok_or_else(|| error(ErrorKind::InvalidArgument, "The position is not valid."))
    })
    .transpose()
}

fn bounds_message(b: Bounds) -> pb::Bounds {
    pb::Bounds {
        west: b.west,
        south: b.south,
        east: b.east,
        north: b.north,
    }
}

fn overlaps(a: &Bounds, b: &Bounds) -> bool {
    a.west <= b.east && b.west <= a.east && a.south <= b.north && b.south <= a.north
}

fn decompress(z: u8, x: u32, y: u32, encoding: TileEncoding, data: Vec<u8>) -> Outcome<Vec<u8>> {
    match encoding {
        TileEncoding::Identity => Ok(data),
        TileEncoding::Gzip => {
            let mut out = Vec::with_capacity(data.len() * 3);
            flate2::read::GzDecoder::new(&data[..])
                .take(16 << 20)
                .read_to_end(&mut out)
                .map_err(|e| error(ErrorKind::InvalidData, format!("Tile {z}/{x}/{y}: {e}")))?;
            Ok(out)
        }
    }
}

/// Opens what is installed for `region`; `None` when nothing is.
fn open_slot(store: &PackageStore, region: Region) -> Option<Slot> {
    let id = region.id;
    let mut problems = Vec::new();
    let dir = store.region_dir(id);
    let has_files = ["active.json", "extras/active.json", "terrain/active.json"]
        .iter()
        .any(|f| dir.join(f).exists());
    if !has_files {
        return None;
    }
    let map = match store.active(id) {
        Ok(Some(active)) => match TilePackage::open(&active.path) {
            Ok(tiles) => Some(OpenMap { active, tiles }),
            Err(e) => {
                problems.push(format!("The {} map could not be opened: {e}.", region.name));
                None
            }
        },
        Ok(None) => None,
        Err(e) => {
            problems.push(format!("The {} map could not be read: {e}.", region.name));
            None
        }
    };
    let mut search = map.as_ref().and_then(|m| {
        SearchIndex::open(&m.active.search_index_path(), &m.active.record.sha256).ok()
    });
    let extras = match active_extras(store, id) {
        Ok(extras) => extras,
        Err(e) => {
            problems.push(format!(
                "The {} stations and addresses could not be read: {e}.",
                region.name
            ));
            None
        }
    };
    let terrain = match active_terrain(store, id) {
        Ok(terrain) => terrain,
        Err(e) => {
            problems.push(format!(
                "The {} terrain could not be read: {e}.",
                region.name
            ));
            None
        }
    };
    let terrain_tiles = terrain
        .as_ref()
        .and_then(|t| match TerrainTiles::open(&t.path) {
            Ok(tiles) => Some(tiles),
            Err(e) => {
                problems.push(format!(
                    "The {} terrain could not be opened: {e}.",
                    region.name
                ));
                None
            }
        });
    if let (Some(index), Some(extras)) = (search.as_mut(), extras.as_ref())
        && let Err(e) = index.attach_extras(&extras.path)
    {
        problems.push(format!(
            "The {} stations and addresses could not be opened for search: {e}.",
            region.name
        ));
    }
    Some(Slot {
        region,
        bounds: map.as_ref().map(|m| m.active.record.metadata.bounds),
        map: Mutex::new(map),
        search: Mutex::new(search),
        extras,
        terrain: terrain_tiles.is_some().then_some(terrain).flatten(),
        terrain_tiles: Mutex::new(terrain_tiles),
        problem: problems.join(" "),
    })
}

impl Service {
    pub fn open(config: &pb::Config) -> Outcome<Self> {
        let root = PathBuf::from(&config.store_directory);
        if config.store_directory.is_empty() || !root.is_absolute() {
            return Err(error(
                ErrorKind::InvalidArgument,
                "The map folder must be an absolute path.",
            ));
        }
        std::fs::create_dir_all(&root).map_err(|e| {
            error(
                ErrorKind::Storage,
                format!(
                    "The map folder {} could not be created: {e}",
                    root.display()
                ),
            )
        })?;
        let leak = |s: &str| -> Option<&'static str> {
            (!s.is_empty()).then(|| &*Box::leak(s.to_owned().into_boxed_str()))
        };
        let places = match SavedPlaces::open(&root) {
            Ok(places) => Places::Ready(places),
            Err(e) => Places::Unreadable(e.to_string()),
        };
        let service = Self {
            store: PackageStore::new(root),
            package_url: leak(&config.package_url),
            extras_url: leak(&config.extras_url),
            terrain_url: match config.terrain_url.as_str() {
                "" => TERRAIN_URL.to_owned(),
                url => url.to_owned(),
            },
            slots: RwLock::new(Vec::new()),
            places: Mutex::new(places),
            progress: Mutex::new(pb::ProgressEvent::default()),
            cancel: AtomicBool::new(false),
            busy: AtomicBool::new(false),
        };
        service.reload();
        Ok(service)
    }

    /// The catalog region `id`, with the test overrides applied to Illinois.
    fn region(&self, id: &str) -> Outcome<Region> {
        let region = *regions::find(id).ok_or_else(|| {
            error(
                ErrorKind::InvalidArgument,
                format!("There is no region {id:?}."),
            )
        })?;
        if region.id != ILLINOIS.id {
            return Ok(region);
        }
        Ok(Region {
            package_url: self.package_url.unwrap_or(region.package_url),
            extras_url: self.extras_url.unwrap_or(region.extras_url),
            ..region
        })
    }

    fn slots(&self) -> Vec<Arc<Slot>> {
        self.slots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_slots(&self, slots: Vec<Arc<Slot>>) {
        *self
            .slots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = slots;
    }

    /// Opens every installed region's map, search index and extras, replacing
    /// what was open. Problems are reported in the status, not as failures, so
    /// a damaged package never prevents downloading a replacement.
    fn reload(&self) {
        // The new set is opened before the old one is closed, so the map never
        // goes blank; replaced versions are closed by the swap.
        let slots = regions::ALL
            .iter()
            .filter_map(|r| self.region(r.id).ok())
            .filter_map(|r| open_slot(&self.store, r))
            .map(Arc::new)
            .collect();
        self.set_slots(slots);
    }

    fn all_bounds(&self) -> Vec<Bounds> {
        self.slots().iter().filter_map(|s| s.bounds).collect()
    }

    pub fn catalog(&self) -> pb::Catalog {
        pb::Catalog {
            regions: regions::ALL
                .iter()
                .map(|r| pb::CatalogRegion {
                    id: r.id.to_owned(),
                    name: r.name.to_owned(),
                    package_bytes: r.package_bytes,
                    extras_bytes: r.extras_bytes,
                    source_page: r.source_page.to_owned(),
                    terrain_bytes: r.terrain_bytes,
                })
                .collect(),
        }
    }

    pub fn status(&self) -> pb::Status {
        let slots = self.slots();
        let mut regions = Vec::new();
        let mut coverage: Option<Bounds> = None;
        let mut terrain_coverage: Option<Bounds> = None;
        let join = |a: Option<Bounds>, b: Bounds| match a {
            None => b,
            Some(c) => Bounds {
                west: c.west.min(b.west),
                south: c.south.min(b.south),
                east: c.east.max(b.east),
                north: c.north.max(b.north),
            },
        };
        let (mut min_zoom, mut max_zoom) = (u32::MAX, 0);
        let mut versions = Vec::new();
        for slot in &slots {
            let map = lock(&slot.map).as_ref().map(|m| {
                let r = &m.active.record;
                let b = r.metadata.bounds;
                coverage = Some(join(coverage, b));
                min_zoom = min_zoom.min(r.metadata.min_zoom.into());
                max_zoom = max_zoom.max(r.metadata.max_zoom.into());
                pb::MapInfo {
                    version: m.active.version.clone(),
                    region_name: slot.region.name.to_owned(),
                    data_date: http_date_to_iso(r.source.last_modified.as_deref()),
                    downloaded_at: r.downloaded_at.clone(),
                    bytes: r.byte_length,
                    bounds: Some(bounds_message(b)),
                    min_zoom: r.metadata.min_zoom.into(),
                    max_zoom: r.metadata.max_zoom.into(),
                    attribution: r.attribution.clone(),
                    source_url: r.source.url.clone(),
                    tiles: r.check.total_tiles,
                }
            });
            let extras = slot.extras.as_ref().map(|x| {
                let r = &x.record;
                pb::ExtrasInfo {
                    version: x.version.clone(),
                    data_date: r
                        .data_timestamp
                        .as_deref()
                        .map(|t| t.chars().take(10).collect())
                        .unwrap_or_else(|| http_date_to_iso(r.source.last_modified.as_deref())),
                    downloaded_at: r.downloaded_at.clone(),
                    fuel: r.stats.fuel,
                    charging: r.stats.charging,
                    addresses: r.stats.addresses,
                    source_bytes: r.byte_length,
                }
            });
            let terrain = slot.terrain.as_ref().map(|t| {
                let r = &t.record;
                terrain_coverage = Some(join(terrain_coverage, r.bounds));
                pb::TerrainInfo {
                    version: t.version.clone(),
                    data_date: http_date_to_iso(r.source.last_modified.as_deref()),
                    downloaded_at: r.downloaded_at.clone(),
                    bytes: r.bytes,
                    tiles: r.tiles,
                    max_zoom: r.max_zoom.into(),
                    attribution: r.attribution.clone(),
                }
            });
            versions.push(format!(
                "{}:{}:{}:{}",
                slot.region.id,
                map.as_ref().map_or("", |m| m.version.as_str()),
                extras.as_ref().map_or("", |x| x.version.as_str()),
                terrain.as_ref().map_or("", |t| t.version.as_str())
            ));
            regions.push(pb::RegionStatus {
                id: slot.region.id.to_owned(),
                name: slot.region.name.to_owned(),
                search_ready: lock(&slot.search).is_some(),
                map,
                extras,
                terrain,
                problem: slot.problem.clone(),
            });
        }
        let bounds = self.all_bounds();
        let (places, places_problem) = match &*lock(&self.places) {
            Places::Ready(list) => (
                list.list()
                    .iter()
                    .map(|p| place_message(p, &bounds))
                    .collect(),
                String::new(),
            ),
            Places::Unreadable(problem) => (Vec::new(), problem.clone()),
        };
        pb::Status {
            search_ready: regions.iter().all(|r| r.map.is_none() || r.search_ready),
            map_problem: regions
                .iter()
                .map(|r| r.problem.as_str())
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
            places,
            places_problem,
            store_directory: self.store.root().display().to_string(),
            updating: self.busy.load(Ordering::Acquire),
            coverage: coverage.map(bounds_message),
            min_zoom: if max_zoom == 0 { 0 } else { min_zoom },
            max_zoom,
            content_version: versions.join(";"),
            terrain_coverage: terrain_coverage.map(bounds_message),
            regions,
        }
    }

    pub fn search(&self, request: &pb::Search) -> Outcome<pb::SearchResults> {
        let near = position(request.near.as_ref())?;
        let limit = match request.limit {
            0 => 20,
            n @ 1..=100 => n as usize,
            _ => {
                return Err(error(
                    ErrorKind::InvalidArgument,
                    "At most 100 results can be requested.",
                ));
            }
        };
        if request.query.chars().count() > 200 {
            return Err(error(ErrorKind::InvalidArgument, "The search is too long."));
        }
        let slots = self.slots();
        // Always in catalog order, so two searches never wait on each other's locks.
        let guards: Vec<_> = slots.iter().map(|s| lock(&s.search)).collect();
        let indexes: Vec<&SearchIndex> = guards.iter().filter_map(|g| g.as_ref()).collect();
        if indexes.is_empty() {
            return Err(error(
                ErrorKind::Unavailable,
                "Search is not available until a map is installed.",
            ));
        }
        let results = search_all(&indexes, &request.query, near, limit)
            .map_err(|e| error(ErrorKind::Internal, format!("Search failed: {e}")))?;
        Ok(pb::SearchResults {
            results: results
                .into_iter()
                .map(|r| pb::SearchResult {
                    name: r.name,
                    kind: r.kind.code().to_owned(),
                    category: r.category,
                    near: r.near.unwrap_or_default(),
                    latitude: r.latitude,
                    longitude: r.longitude,
                    reference: r.reference.unwrap_or_default(),
                    has_distance: r.distance_km.is_some(),
                    distance_km: r.distance_km.unwrap_or_default(),
                    detail: r.detail.unwrap_or_default(),
                })
                .collect(),
        })
    }

    /// The tile at z/x/y, decompressed; `None` where no installed map has one.
    /// A tile on a border between installed regions is merged from each.
    pub fn tile(&self, z: u32, x: u32, y: u32) -> Outcome<Option<Vec<u8>>> {
        let z = u8::try_from(z)
            .ok()
            .filter(|z| *z <= 24)
            .ok_or_else(|| error(ErrorKind::InvalidArgument, "Invalid zoom."))?;
        let n = 1_u64 << z;
        if u64::from(x) >= n || u64::from(y) >= n {
            return Ok(None);
        }
        let (west, north) = tile_point_to_lon_lat(z, x, y, 4096, 0, 0);
        let (east, south) = tile_point_to_lon_lat(z, x, y, 4096, 4096, 4096);
        let area = Bounds {
            west,
            south,
            east,
            north,
        };
        let mut found = Vec::new();
        for slot in self.slots() {
            if !slot.bounds.is_some_and(|b| overlaps(&b, &area)) {
                continue;
            }
            let guard = lock(&slot.map);
            let Some(map) = guard.as_ref() else { continue };
            let tile = map
                .tiles
                .tile(z, x, y)
                .map_err(|e| error(ErrorKind::InvalidData, format!("Tile {z}/{x}/{y}: {e}")))?;
            drop(guard);
            if let Some(tile) = tile {
                found.push(decompress(z, x, y, tile.encoding, tile.data)?);
            }
        }
        match found.len() {
            0 => Ok(None),
            1 => Ok(found.pop()),
            _ => {
                let parts: Vec<&[u8]> = found.iter().map(Vec::as_slice).collect();
                merge_tiles(&parts).map(Some).map_err(|e| {
                    error(
                        ErrorKind::InvalidData,
                        format!("Tile {z}/{x}/{y} could not be merged: {e}"),
                    )
                })
            }
        }
    }

    /// The terrain tile at z/x/y from whichever installed region has it.
    pub fn terrain_tile(&self, request: &pb::GetTerrainTile) -> Outcome<pb::TerrainTile> {
        let (x, y) = (request.x, request.y);
        let Some(z) = u8::try_from(request.z)
            .ok()
            .filter(|z| *z <= TERRAIN_MAX_ZOOM)
        else {
            return Ok(pb::TerrainTile::default());
        };
        let n = 1_u64 << z;
        if u64::from(x) >= n || u64::from(y) >= n {
            return Ok(pb::TerrainTile::default());
        }
        let (west, north) = tile_point_to_lon_lat(z, x, y, 4096, 0, 0);
        let (east, south) = tile_point_to_lon_lat(z, x, y, 4096, 4096, 4096);
        let area = Bounds {
            west,
            south,
            east,
            north,
        };
        for slot in self.slots() {
            if !slot
                .terrain
                .as_ref()
                .is_some_and(|t| overlaps(&t.record.bounds, &area))
            {
                continue;
            }
            let guard = lock(&slot.terrain_tiles);
            let Some(tiles) = guard.as_ref() else {
                continue;
            };
            let found = tiles.tile(z, x, y).map_err(|e| {
                error(
                    ErrorKind::InvalidData,
                    format!("Terrain tile {z}/{x}/{y}: {e}"),
                )
            })?;
            if let Some(data) = found {
                return Ok(pb::TerrainTile { data });
            }
        }
        Ok(pb::TerrainTile::default())
    }

    pub fn stations(&self) -> Outcome<pb::Stations> {
        let slots = self.slots();
        let paths: Vec<&Path> = slots
            .iter()
            .filter_map(|s| s.extras.as_ref().map(|x| x.path.as_path()))
            .collect();
        let geojson = stations_geojson_all(&paths).map_err(|e| {
            error(
                ErrorKind::InvalidData,
                format!("Stations could not be read: {e}"),
            )
        })?;
        Ok(pb::Stations { geojson })
    }

    fn with_places<T>(&self, f: impl FnOnce(&mut SavedPlaces) -> Outcome<T>) -> Outcome<T> {
        match &mut *lock(&self.places) {
            Places::Ready(places) => f(places),
            Places::Unreadable(problem) => Err(error(ErrorKind::InvalidData, problem.clone())),
        }
    }

    pub fn add_place(&self, request: &pb::AddPlace) -> Outcome<pb::Place> {
        let at = position(request.position.as_ref())?
            .ok_or_else(|| error(ErrorKind::InvalidArgument, "A position is required."))?;
        let bounds = self.all_bounds();
        self.with_places(|places| {
            places
                .add(&request.name, &request.note, at)
                .map(|p| place_message(&p, &bounds))
                .map_err(places_error)
        })
    }

    pub fn update_place(&self, request: &pb::UpdatePlace) -> Outcome<pb::Place> {
        let bounds = self.all_bounds();
        self.with_places(|places| {
            places
                .update(request.id, &request.name, &request.note)
                .map_err(places_error)?;
            let place = places
                .get(request.id)
                .ok_or_else(|| places_error(PlacesError::NotFound(request.id)))?;
            Ok(place_message(place, &bounds))
        })
    }

    pub fn remove_place(&self, id: u64) -> Outcome<()> {
        self.with_places(|places| places.remove(id).map_err(places_error))
    }

    /// Keeps the unreadable file under a new name and starts an empty list.
    pub fn reset_places(&self) -> Outcome<()> {
        let mut guard = lock(&self.places);
        if let Places::Unreadable(_) = &*guard {
            SavedPlaces::move_aside_unreadable(self.store.root())
                .map_err(|e| error(ErrorKind::Storage, e.to_string()))?;
            let places = SavedPlaces::open(self.store.root()).map_err(places_error)?;
            *guard = Places::Ready(places);
        }
        Ok(())
    }

    fn begin(&self, target: pb::UpdateTarget, region: Option<&Region>) -> Outcome<BusyGuard<'_>> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(error(
                ErrorKind::Busy,
                "Another map update is already running.",
            ));
        }
        self.cancel.store(false, Ordering::Release);
        *lock(&self.progress) = pb::ProgressEvent {
            active: true,
            target: target as i32,
            phase: "Checking".into(),
            region_id: region.map(|r| r.id.to_owned()).unwrap_or_default(),
            region_name: region.map(|r| r.name.to_owned()).unwrap_or_default(),
            ..Default::default()
        };
        Ok(BusyGuard(self))
    }

    fn report(&self, progress: Progress) {
        let (phase, done, total) = match progress {
            Progress::Checking => ("Checking", 0, 0),
            Progress::Downloading { received, total } => {
                ("Downloading", received, total.unwrap_or(0))
            }
            Progress::Verifying => ("Verifying", 0, 0),
            Progress::Indexing { done, total } => ("Preparing", done, total),
            Progress::Activating => ("Activating", 0, 0),
        };
        let mut state = lock(&self.progress);
        state.phase = phase.into();
        state.done = done;
        state.total = total;
    }

    pub fn progress(&self) -> pb::ProgressEvent {
        lock(&self.progress).clone()
    }

    pub fn cancel(&self) {
        if self.busy.load(Ordering::Acquire) {
            self.cancel.store(true, Ordering::Release);
            lock(&self.progress).cancelling = true;
        }
    }

    pub fn update(&self, request: &pb::Update) -> Outcome<pb::UpdateResult> {
        let target = pb::UpdateTarget::try_from(request.target)
            .ok()
            .filter(|t| *t != pb::UpdateTarget::Unspecified)
            .ok_or_else(|| error(ErrorKind::InvalidArgument, "No update target."))?;
        let region = self.region(&request.region_id)?;
        let _busy = self.begin(target, Some(&region))?;
        let transport = HttpTransport::new();
        let options = UpdateOptions::default();
        let id = region.id;
        let result = match target {
            pb::UpdateTarget::Map => update_region(
                &self.store,
                &region,
                &transport,
                &options,
                &mut |p| self.report(p),
                &self.cancel,
            )
            .map(|outcome| match outcome {
                UpdateOutcome::UpToDate(active) => pb::UpdateResult {
                    up_to_date: true,
                    version: active.version,
                    replaced: String::new(),
                },
                UpdateOutcome::Activated {
                    package, replaced, ..
                } => pb::UpdateResult {
                    up_to_date: false,
                    version: package.version,
                    replaced: replaced.unwrap_or_default(),
                },
            }),
            pb::UpdateTarget::Terrain => {
                let bounds = self
                    .slots()
                    .iter()
                    .find(|s| s.region.id == id)
                    .and_then(|s| s.bounds)
                    .ok_or_else(|| {
                        error(
                            ErrorKind::NotFound,
                            format!("Download the {} map before its terrain.", region.name),
                        )
                    })?;
                update_terrain(
                    &self.store,
                    &region,
                    bounds,
                    &self.terrain_url,
                    TERRAIN_MAX_ZOOM,
                    &transport,
                    &options,
                    &mut |p| self.report(p),
                    &self.cancel,
                )
                .map(|outcome| match outcome {
                    TerrainOutcome::UpToDate(active) => pb::UpdateResult {
                        up_to_date: true,
                        version: active.version,
                        replaced: String::new(),
                    },
                    TerrainOutcome::Activated { terrain, replaced } => pb::UpdateResult {
                        up_to_date: false,
                        version: terrain.version,
                        replaced: replaced.unwrap_or_default(),
                    },
                })
            }
            _ => update_extras(
                &self.store,
                &region,
                &transport,
                &options,
                &mut |p| self.report(p),
                &self.cancel,
            )
            .map(|outcome| match outcome {
                ExtrasOutcome::UpToDate(active) => pb::UpdateResult {
                    up_to_date: true,
                    version: active.version,
                    replaced: String::new(),
                },
                ExtrasOutcome::Activated {
                    extras, replaced, ..
                } => pb::UpdateResult {
                    up_to_date: false,
                    version: extras.version,
                    replaced: replaced.unwrap_or_default(),
                },
            }),
        }
        .map_err(update_error)?;
        if !result.up_to_date {
            // Close the replaced files, then remove them.
            self.reload();
            match target {
                pb::UpdateTarget::Map => {
                    self.store.prune(id);
                }
                pb::UpdateTarget::Terrain => {
                    prune_terrain(&self.store, id);
                }
                _ => {
                    prune_extras(&self.store, id);
                }
            }
        }
        Ok(result)
    }

    /// Deletes a region's map, search index and extras. Saved places are kept.
    pub fn remove_region(&self, request: &pb::RemoveRegion) -> Outcome<()> {
        let region = self.region(&request.region_id)?;
        let _busy = self.begin(pb::UpdateTarget::Map, Some(&region))?;
        lock(&self.progress).phase = "Removing".into();
        let dir = self.store.region_dir(region.id);
        let kept: Vec<Arc<Slot>> = self
            .slots()
            .into_iter()
            .filter(|s| s.region.id != region.id)
            .collect();
        self.set_slots(kept);
        // A tile read that started before the swap may hold the files briefly.
        let mut last = None;
        for attempt in 0..20 {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    last = None;
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(25 * (attempt + 1)));
                }
            }
        }
        let outcome = match last {
            None => Ok(()),
            Some(e) => Err(error(
                ErrorKind::Storage,
                format!("The {} map could not be removed: {e}", region.name),
            )),
        };
        // Reopen whatever is left of it if removal failed part-way.
        self.reload();
        outcome
    }

    /// Builds the search index of every installed map that has none
    /// (packages installed before search existed).
    pub fn prepare_search(&self) -> Outcome<()> {
        let missing: Vec<(Region, ActivePackage)> = self
            .slots()
            .iter()
            .filter(|s| lock(&s.search).is_none())
            .filter_map(|s| lock(&s.map).as_ref().map(|m| (s.region, m.active.clone())))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        for (region, active) in missing {
            let _busy = self.begin(pb::UpdateTarget::Map, Some(&region))?;
            open_or_build(&active, &self.cancel, &mut |done, total| {
                self.report(Progress::Indexing { done, total })
            })
            .map_err(|e| match e {
                offline_maps_core::mbtiles::PackageError::Io(io)
                    if io.kind() == std::io::ErrorKind::Interrupted =>
                {
                    error(ErrorKind::Cancelled, "Preparing search was cancelled.")
                }
                other => error(
                    ErrorKind::InvalidData,
                    format!("Search for {} could not be prepared: {other}", region.name),
                ),
            })?;
        }
        self.reload();
        Ok(())
    }
}
