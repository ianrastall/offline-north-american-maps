use super::*;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "offline-maps-ffi-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn read(result: *mut ResultBuffer) -> Vec<u8> {
    let mut bytes = ptr::null();
    let mut length = 0;
    unsafe {
        assert_eq!(om_result_bytes(result, &mut bytes, &mut length), STATUS_OK);
        let copy = std::slice::from_raw_parts(bytes, length).to_vec();
        om_result_free(result);
        copy
    }
}

fn create(store: &std::path::Path) -> *mut Core {
    let config = pb::Config {
        store_directory: store.display().to_string(),
        ..Default::default()
    }
    .encode_to_vec();
    let (mut core, mut error) = (ptr::null_mut(), ptr::null_mut());
    let status = unsafe { om_create(config.as_ptr(), config.len(), &mut core, &mut error) };
    assert_eq!(status, STATUS_OK);
    assert!(error.is_null() && !core.is_null());
    core
}

fn call(core: *const Core, id: u64, command: pb::request::Command) -> pb::Response {
    let request = pb::Request {
        request_id: id,
        command: Some(command),
    }
    .encode_to_vec();
    let mut result = ptr::null_mut();
    let status = unsafe { om_call(core, request.as_ptr(), request.len(), &mut result) };
    assert_eq!(status, STATUS_OK);
    let response = pb::Response::decode(&read(result)[..]).unwrap();
    assert_eq!(response.request_id, id);
    response
}

fn status(core: *const Core) -> pb::Status {
    match call(core, 1, pb::request::Command::GetStatus(pb::GetStatus {})).result {
        Some(pb::response::Result::Status(s)) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn empty_store_reports_no_map_and_refuses_search() {
    let dir = TempDir::new();
    let core = create(&dir.0);
    let s = status(core);
    assert!(s.regions.is_empty() && s.coverage.is_none() && !s.updating);
    assert!(s.content_version.is_empty());
    assert!(dir.0.is_dir());

    let catalog = call(core, 6, pb::request::Command::GetCatalog(pb::GetCatalog {}));
    let Some(pb::response::Result::Catalog(catalog)) = catalog.result else {
        panic!("{catalog:?}");
    };
    assert_eq!(catalog.regions.len(), 52);
    let illinois = catalog.regions.iter().find(|r| r.id == "us-il").unwrap();
    assert_eq!(illinois.name, "Illinois");
    assert!(illinois.package_bytes > 100_000_000);
    assert!(illinois.terrain_bytes > 100_000_000);

    // No terrain installed: an empty tile; terrain needs the region's map first.
    let tile = call(
        core,
        13,
        pb::request::Command::GetTerrainTile(pb::GetTerrainTile { z: 12, x: 1, y: 1 }),
    );
    match tile.result {
        Some(pb::response::Result::TerrainTile(t)) => assert!(t.data.is_empty()),
        other => panic!("{other:?}"),
    }
    let terrain = call(
        core,
        14,
        pb::request::Command::Update(pb::Update {
            target: pb::UpdateTarget::Terrain as i32,
            region_id: "us-il".into(),
        }),
    );
    assert_eq!(terrain.error.unwrap().kind, ErrorKind::NotFound as i32);

    // Removing a region that is not installed succeeds; an unknown one does not.
    let removed = call(
        core,
        11,
        pb::request::Command::RemoveRegion(pb::RemoveRegion {
            region_id: "us-in".into(),
        }),
    );
    assert!(removed.error.is_none(), "{removed:?}");
    let unknown = call(
        core,
        12,
        pb::request::Command::RemoveRegion(pb::RemoveRegion {
            region_id: "us-ak".into(),
        }),
    );
    assert_eq!(
        unknown.error.unwrap().kind,
        ErrorKind::InvalidArgument as i32
    );

    let search = call(
        core,
        7,
        pb::request::Command::Search(pb::Search {
            query: "springfield".into(),
            ..Default::default()
        }),
    );
    assert_eq!(search.error.unwrap().kind, ErrorKind::Unavailable as i32);

    // No map: an empty tile, not a failure.
    let mut result = ptr::null_mut();
    assert_eq!(
        unsafe { om_tile(core, 14, 4203, 6089, &mut result) },
        STATUS_OK
    );
    assert!(read(result).is_empty());
    assert_eq!(
        unsafe { om_tile(core, 99, 0, 0, &mut result) },
        STATUS_INVALID_ARGUMENT
    );
    assert!(result.is_null());

    let stations = call(
        core,
        8,
        pb::request::Command::GetStations(pb::GetStations {}),
    );
    match stations.result {
        Some(pb::response::Result::Stations(s)) => assert!(s.geojson.contains("FeatureCollection")),
        other => panic!("{other:?}"),
    }
    unsafe { om_destroy(core) };
}

#[test]
fn saved_places_round_trip_and_persist() {
    let dir = TempDir::new();
    let core = create(&dir.0);
    let added = call(
        core,
        2,
        pb::request::Command::AddPlace(pb::AddPlace {
            name: "Home".into(),
            note: "Spare key with neighbour".into(),
            position: Some(pb::Position {
                latitude: 40.15,
                longitude: -88.96,
            }),
        }),
    );
    let Some(pb::response::Result::Place(place)) = added.result else {
        panic!("{added:?}");
    };
    assert_eq!(
        (place.id, place.name.as_str(), place.covered),
        (1, "Home", false)
    );

    let invalid = call(
        core,
        3,
        pb::request::Command::AddPlace(pb::AddPlace {
            name: " ".into(),
            position: Some(pb::Position {
                latitude: 40.0,
                longitude: -89.0,
            }),
            ..Default::default()
        }),
    );
    assert_eq!(
        invalid.error.unwrap().kind,
        ErrorKind::InvalidArgument as i32
    );
    let off_earth = call(
        core,
        4,
        pb::request::Command::AddPlace(pb::AddPlace {
            name: "x".into(),
            position: Some(pb::Position {
                latitude: 95.0,
                longitude: 0.0,
            }),
            ..Default::default()
        }),
    );
    assert_eq!(
        off_earth.error.unwrap().kind,
        ErrorKind::InvalidArgument as i32
    );

    call(
        core,
        5,
        pb::request::Command::UpdatePlace(pb::UpdatePlace {
            id: 1,
            name: "Home (Clinton)".into(),
            note: String::new(),
        }),
    );
    unsafe { om_destroy(core) };

    let core = create(&dir.0);
    let places = status(core).places;
    assert_eq!(places.len(), 1);
    assert_eq!(places[0].name, "Home (Clinton)");
    let removed = call(
        core,
        6,
        pb::request::Command::RemovePlace(pb::RemovePlace { id: 1 }),
    );
    assert!(removed.error.is_none());
    let again = call(
        core,
        7,
        pb::request::Command::RemovePlace(pb::RemovePlace { id: 1 }),
    );
    assert_eq!(again.error.unwrap().kind, ErrorKind::NotFound as i32);
    unsafe { om_destroy(core) };
}

#[test]
fn unreadable_places_are_kept_until_reset() {
    let dir = TempDir::new();
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(dir.0.join("saved-places.json"), b"{ damaged").unwrap();
    let core = create(&dir.0);
    let s = status(core);
    assert!(s.places_problem.contains("cannot be read"));
    assert_eq!(
        std::fs::read(dir.0.join("saved-places.json")).unwrap(),
        b"{ damaged"
    );
    let reset = call(
        core,
        2,
        pb::request::Command::ResetPlaces(pb::ResetPlaces {}),
    );
    assert!(reset.error.is_none());
    assert!(status(core).places_problem.is_empty());
    let kept = std::fs::read_dir(&dir.0).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("saved-places.unreadable-")
    });
    assert!(kept);
    unsafe { om_destroy(core) };
}

#[test]
fn transport_contract() {
    unsafe {
        assert_eq!(om_abi_version(), ABI_VERSION);
        let (mut core, mut error) = (ptr::null_mut(), ptr::null_mut());
        // Relative folder: a typed creation error.
        let config = pb::Config {
            store_directory: "relative".into(),
            ..Default::default()
        }
        .encode_to_vec();
        assert_eq!(
            om_create(config.as_ptr(), config.len(), &mut core, &mut error),
            STATUS_CREATE_FAILED
        );
        assert!(core.is_null());
        let response = pb::Response::decode(&read(error)[..]).unwrap();
        assert_eq!(
            response.error.unwrap().kind,
            ErrorKind::InvalidArgument as i32
        );
        // Garbage config.
        let garbage = [0xff_u8; 8];
        assert_eq!(
            om_create(garbage.as_ptr(), garbage.len(), &mut core, &mut error),
            STATUS_MALFORMED_REQUEST
        );
        assert!(core.is_null() && error.is_null());
        assert_eq!(
            om_create(ptr::null(), 4, &mut core, &mut error),
            STATUS_INVALID_ARGUMENT
        );

        let dir = TempDir::new();
        let core = create(&dir.0);
        let mut result = ptr::null_mut();
        assert_eq!(
            om_call(core, garbage.as_ptr(), garbage.len(), &mut result),
            STATUS_MALFORMED_REQUEST
        );
        assert!(result.is_null());
        assert_eq!(
            om_call(core, ptr::null(), 1, &mut result),
            STATUS_INVALID_ARGUMENT
        );
        let huge = vec![0_u8; MAX_REQUEST_BYTES + 1];
        assert_eq!(
            om_call(core, huge.as_ptr(), huge.len(), &mut result),
            STATUS_INVALID_ARGUMENT
        );
        assert_eq!(
            om_call(ptr::null(), garbage.as_ptr(), 0, &mut result),
            STATUS_INVALID_ARGUMENT
        );
        // An empty request decodes but has no command: a typed error.
        assert_eq!(om_call(core, ptr::null(), 0, &mut result), STATUS_OK);
        let response = pb::Response::decode(&read(result)[..]).unwrap();
        assert_eq!(
            response.error.unwrap().kind,
            ErrorKind::InvalidArgument as i32
        );
        // Unknown update target.
        let bad_target = pb::Request {
            request_id: 9,
            command: Some(pb::request::Command::Update(pb::Update {
                target: 42,
                region_id: "us-il".into(),
            })),
        }
        .encode_to_vec();
        assert_eq!(
            om_call(core, bad_target.as_ptr(), bad_target.len(), &mut result),
            STATUS_OK
        );
        let response = pb::Response::decode(&read(result)[..]).unwrap();
        assert_eq!(
            response.error.unwrap().kind,
            ErrorKind::InvalidArgument as i32
        );

        // Progress and cancel without an update.
        assert_eq!(om_progress(core, &mut result), STATUS_OK);
        let progress = pb::ProgressEvent::decode(&read(result)[..]).unwrap();
        assert!(!progress.active);
        assert_eq!(om_cancel(core), STATUS_OK);
        assert_eq!(om_cancel(ptr::null()), STATUS_INVALID_ARGUMENT);

        // A panic marks the core unusable for normal calls only.
        assert_eq!(guarded(core, |_| panic!("test panic")), STATUS_PANIC);
        assert_eq!(om_call(core, ptr::null(), 0, &mut result), STATUS_UNUSABLE);
        assert_eq!(om_progress(core, &mut result), STATUS_OK);
        om_result_free(result);
        om_result_free(ptr::null_mut());
        om_destroy(core);
        om_destroy(ptr::null_mut());
    }
}

#[test]
fn update_against_an_unreachable_server_fails_cleanly() {
    let dir = TempDir::new();
    let config = pb::Config {
        store_directory: dir.0.display().to_string(),
        // Nothing listens on port 9 (discard) on loopback.
        package_url: "http://127.0.0.1:9/package.mbtiles".into(),
        ..Default::default()
    }
    .encode_to_vec();
    let (mut core, mut error) = (ptr::null_mut(), ptr::null_mut());
    unsafe { om_create(config.as_ptr(), config.len(), &mut core, &mut error) };
    let response = call(
        core,
        3,
        pb::request::Command::Update(pb::Update {
            target: pb::UpdateTarget::Map as i32,
            region_id: "us-il".into(),
        }),
    );
    assert_eq!(response.error.unwrap().kind, ErrorKind::Network as i32);
    let s = status(core);
    assert!(s.regions.is_empty() && !s.updating);
    // A region outside the catalog is refused before anything is downloaded.
    let response = call(
        core,
        4,
        pb::request::Command::Update(pb::Update {
            target: pb::UpdateTarget::Map as i32,
            region_id: "us-zz".into(),
        }),
    );
    assert_eq!(
        response.error.unwrap().kind,
        ErrorKind::InvalidArgument as i32
    );
    unsafe { om_destroy(core) };
}

/// Runs against a real store when `OFFLINE_MAPS_TEST_STORE` names one (for
/// example data/store-index after `offline-maps-dev download`).
#[test]
fn real_store_when_available() {
    let Ok(store) = std::env::var("OFFLINE_MAPS_TEST_STORE") else {
        return;
    };
    let core = create(std::path::Path::new(&store));
    let s = status(core);
    let illinois = s
        .regions
        .iter()
        .find(|r| r.id == "us-il")
        .expect("the test store has Illinois");
    let map = illinois.map.as_ref().expect("the test store has a map");
    assert!(s.search_ready, "{}", s.map_problem);
    assert_eq!(map.max_zoom, 14);
    assert_eq!(s.max_zoom, 14);
    assert!(s.coverage.is_some() && !s.content_version.is_empty());
    let mut result = ptr::null_mut();
    assert_eq!(
        unsafe { om_tile(core, 14, 4203, 6089, &mut result) },
        STATUS_OK
    );
    let tile = read(result);
    assert!(tile.len() > 1000 && tile[0] != 0x1f, "decompressed MVT");
    let found = call(
        core,
        2,
        pb::request::Command::Search(pb::Search {
            query: "springfield".into(),
            limit: 3,
            ..Default::default()
        }),
    );
    let Some(pb::response::Result::Search(results)) = found.result else {
        panic!("{found:?}");
    };
    assert_eq!(results.results[0].name, "Springfield");
    assert_eq!(results.results[0].kind, "place");
    unsafe { om_destroy(core) };
}
