//! The command layer behind the C ABI: one region's map package, extras, search
//! and saved places. Safe to call from several threads at once: tile reads,
//! searches and saved-place changes each take their own short lock, and an
//! update runs without holding any of them, so the map stays usable while a
//! new package downloads. Only one update or search preparation runs at a time.
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use offline_maps_core::Coordinates;
use offline_maps_core::download::{
    HttpTransport, Progress, UpdateError, UpdateOptions, UpdateOutcome, update_region,
};
use offline_maps_core::extras::{
    ActiveExtras, ExtrasOutcome, active_extras, prune_extras, stations_geojson, update_extras,
};
use offline_maps_core::mbtiles::{Bounds, TileEncoding, TilePackage};
use offline_maps_core::package::{ILLINOIS, Region};
use offline_maps_core::places::{PlacesError, SavedPlace, SavedPlaces};
use offline_maps_core::search::{SearchIndex, open_or_build};
use offline_maps_core::store::{ActivePackage, PackageStore};

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

enum Places {
    Ready(SavedPlaces),
    Unreadable(String),
}

pub struct Service {
    store: PackageStore,
    region: Region,
    map: Mutex<Option<OpenMap>>,
    search: Mutex<Option<SearchIndex>>,
    extras: Mutex<Option<ActiveExtras>>,
    problem: Mutex<String>,
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

fn covered(place: &SavedPlace, bounds: Option<&Bounds>) -> bool {
    bounds.is_some_and(|b| place.is_covered_by(b))
}

fn place_message(place: &SavedPlace, bounds: Option<&Bounds>) -> pb::Place {
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
        let leak = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
        let region = Region {
            package_url: match config.package_url.as_str() {
                "" => ILLINOIS.package_url,
                url => leak(url),
            },
            extras_url: match config.extras_url.as_str() {
                "" => ILLINOIS.extras_url,
                url => leak(url),
            },
            ..ILLINOIS
        };
        let places = match SavedPlaces::open(&root) {
            Ok(places) => Places::Ready(places),
            Err(e) => Places::Unreadable(e.to_string()),
        };
        let service = Self {
            store: PackageStore::new(root),
            region,
            map: Mutex::new(None),
            search: Mutex::new(None),
            extras: Mutex::new(None),
            problem: Mutex::new(String::new()),
            places: Mutex::new(places),
            progress: Mutex::new(pb::ProgressEvent::default()),
            cancel: AtomicBool::new(false),
            busy: AtomicBool::new(false),
        };
        service.reload();
        Ok(service)
    }

    /// Opens the active map, its search index and the extras, replacing what
    /// was open. Problems are reported in the status, not as failures, so a
    /// damaged package never prevents downloading a replacement.
    fn reload(&self) {
        let id = self.region.id;
        let mut problems = Vec::new();
        let map = match self.store.active(id) {
            Ok(Some(active)) => match TilePackage::open(&active.path) {
                Ok(tiles) => Some(OpenMap { active, tiles }),
                Err(e) => {
                    problems.push(format!("The installed map could not be opened: {e}."));
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                problems.push(format!("The installed map could not be read: {e}."));
                None
            }
        };
        let mut search = map.as_ref().and_then(|m| {
            SearchIndex::open(&m.active.search_index_path(), &m.active.record.sha256).ok()
        });
        let extras = match active_extras(&self.store, id) {
            Ok(extras) => extras,
            Err(e) => {
                problems.push(format!(
                    "The installed stations and addresses could not be read: {e}."
                ));
                None
            }
        };
        if let (Some(index), Some(extras)) = (search.as_mut(), extras.as_ref())
            && let Err(e) = index.attach_extras(&extras.path)
        {
            problems.push(format!(
                "Stations and addresses could not be opened for search: {e}."
            ));
        }
        *lock(&self.map) = map;
        *lock(&self.search) = search;
        *lock(&self.extras) = extras;
        *lock(&self.problem) = problems.join(" ");
    }

    fn bounds(&self) -> Option<Bounds> {
        lock(&self.map)
            .as_ref()
            .map(|m| m.active.record.metadata.bounds)
    }

    pub fn status(&self) -> pb::Status {
        let map = lock(&self.map).as_ref().map(|m| {
            let r = &m.active.record;
            let b = r.metadata.bounds;
            pb::MapInfo {
                version: m.active.version.clone(),
                region_name: self.region.name.to_owned(),
                data_date: http_date_to_iso(r.source.last_modified.as_deref()),
                downloaded_at: r.downloaded_at.clone(),
                bytes: r.byte_length,
                bounds: Some(pb::Bounds {
                    west: b.west,
                    south: b.south,
                    east: b.east,
                    north: b.north,
                }),
                min_zoom: r.metadata.min_zoom.into(),
                max_zoom: r.metadata.max_zoom.into(),
                attribution: r.attribution.clone(),
                source_url: r.source.url.clone(),
                tiles: r.check.total_tiles,
            }
        });
        let extras = lock(&self.extras).as_ref().map(|x| {
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
        let bounds = self.bounds();
        let (places, places_problem) = match &*lock(&self.places) {
            Places::Ready(list) => (
                list.list()
                    .iter()
                    .map(|p| place_message(p, bounds.as_ref()))
                    .collect(),
                String::new(),
            ),
            Places::Unreadable(problem) => (Vec::new(), problem.clone()),
        };
        pb::Status {
            map,
            extras,
            places,
            search_ready: lock(&self.search).is_some(),
            map_problem: lock(&self.problem).clone(),
            places_problem,
            store_directory: self.store.root().display().to_string(),
            region_name: self.region.name.to_owned(),
            updating: self.busy.load(Ordering::Acquire),
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
        let guard = lock(&self.search);
        let Some(index) = guard.as_ref() else {
            return Err(error(
                ErrorKind::Unavailable,
                "Search is not available until a map is installed.",
            ));
        };
        let results = index
            .search(&request.query, near, limit)
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

    /// The tile at z/x/y, decompressed; `None` where the package has none.
    pub fn tile(&self, z: u32, x: u32, y: u32) -> Outcome<Option<Vec<u8>>> {
        let z = u8::try_from(z)
            .ok()
            .filter(|z| *z <= 24)
            .ok_or_else(|| error(ErrorKind::InvalidArgument, "Invalid zoom."))?;
        let guard = lock(&self.map);
        let Some(map) = guard.as_ref() else {
            return Ok(None);
        };
        let tile = map
            .tiles
            .tile(z, x, y)
            .map_err(|e| error(ErrorKind::InvalidData, format!("Tile {z}/{x}/{y}: {e}")))?;
        drop(guard);
        let Some(tile) = tile else { return Ok(None) };
        match tile.encoding {
            TileEncoding::Identity => Ok(Some(tile.data)),
            TileEncoding::Gzip => {
                let mut out = Vec::with_capacity(tile.data.len() * 3);
                flate2::read::GzDecoder::new(&tile.data[..])
                    .take(16 << 20)
                    .read_to_end(&mut out)
                    .map_err(|e| error(ErrorKind::InvalidData, format!("Tile {z}/{x}/{y}: {e}")))?;
                Ok(Some(out))
            }
        }
    }

    pub fn stations(&self) -> Outcome<pb::Stations> {
        let path = lock(&self.extras).as_ref().map(|x| x.path.clone());
        let geojson = match path {
            Some(path) => stations_geojson(&path).map_err(|e| {
                error(
                    ErrorKind::InvalidData,
                    format!("Stations could not be read: {e}"),
                )
            })?,
            None => r#"{"type":"FeatureCollection","features":[]}"#.to_owned(),
        };
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
        let bounds = self.bounds();
        self.with_places(|places| {
            places
                .add(&request.name, &request.note, at)
                .map(|p| place_message(&p, bounds.as_ref()))
                .map_err(places_error)
        })
    }

    pub fn update_place(&self, request: &pb::UpdatePlace) -> Outcome<pb::Place> {
        let bounds = self.bounds();
        self.with_places(|places| {
            places
                .update(request.id, &request.name, &request.note)
                .map_err(places_error)?;
            let place = places
                .get(request.id)
                .ok_or_else(|| places_error(PlacesError::NotFound(request.id)))?;
            Ok(place_message(place, bounds.as_ref()))
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

    fn begin(&self, target: pb::UpdateTarget) -> Outcome<BusyGuard<'_>> {
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

    pub fn update(&self, target: pb::UpdateTarget) -> Outcome<pb::UpdateResult> {
        let _busy = self.begin(target)?;
        let transport = HttpTransport::new();
        let options = UpdateOptions::default();
        let id = self.region.id;
        let result = match target {
            pb::UpdateTarget::Map => update_region(
                &self.store,
                &self.region,
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
            pb::UpdateTarget::Extras => update_extras(
                &self.store,
                &self.region,
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
            pb::UpdateTarget::Unspecified => {
                return Err(error(ErrorKind::InvalidArgument, "No update target."));
            }
        }
        .map_err(update_error)?;
        if !result.up_to_date {
            // Close the replaced files, then remove them.
            self.reload();
            match target {
                pb::UpdateTarget::Map => {
                    self.store.prune(id);
                }
                _ => {
                    prune_extras(&self.store, id);
                }
            }
        }
        Ok(result)
    }

    /// Builds the active map's missing search index (packages installed before
    /// search existed).
    pub fn prepare_search(&self) -> Outcome<()> {
        let _busy = self.begin(pb::UpdateTarget::Map)?;
        let active = lock(&self.map)
            .as_ref()
            .map(|m| m.active.clone())
            .ok_or_else(|| error(ErrorKind::NotFound, "No map is installed."))?;
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
                format!("Search could not be prepared: {other}"),
            ),
        })?;
        self.reload();
        Ok(())
    }
}
