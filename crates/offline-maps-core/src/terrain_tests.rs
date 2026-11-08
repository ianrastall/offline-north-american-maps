use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::download::tests::options;
use crate::download::{Fetched, RemoteInfo};
use crate::package::ILLINOIS;
use crate::pmtiles::{HEADER_LENGTH, tile_id};
use crate::store::tests::TempDir;

const URL: &str = "https://terrain.test/planet.pmtiles";

/// A west/south/east/north box touching tile 0/0/0, 1/0/0 and 2/0/1, 2/1/1.
const AREA: Bounds = Bounds {
    west: -100.0,
    south: 30.0,
    east: -80.0,
    north: 45.0,
};

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn tile_bytes(z: u8, x: u32, y: u32, version: u8) -> Vec<u8> {
    format!("webp {z}/{x}/{y} v{version}").into_bytes()
}

/// A PMTiles archive with every tile of zooms 0–2, uncompressed directory.
fn archive(version: u8) -> Vec<u8> {
    let mut tiles = Vec::new();
    for z in 0..=2_u8 {
        for x in 0..1_u32 << z {
            for y in 0..1_u32 << z {
                tiles.push((tile_id(z, x, y), tile_bytes(z, x, y, version)));
            }
        }
    }
    tiles.sort();
    let mut dir = Vec::new();
    varint(tiles.len() as u64, &mut dir);
    let mut last = 0;
    for (id, _) in &tiles {
        varint(id - last, &mut dir);
        last = *id;
    }
    for _ in &tiles {
        varint(1, &mut dir);
    }
    for (_, data) in &tiles {
        varint(data.len() as u64, &mut dir);
    }
    for (i, _) in tiles.iter().enumerate() {
        varint(if i == 0 { 1 } else { 0 }, &mut dir);
    }
    let data: Vec<u8> = tiles.iter().flat_map(|(_, d)| d.clone()).collect();
    let mut header = vec![0_u8; HEADER_LENGTH as usize];
    header[..7].copy_from_slice(b"PMTiles");
    header[7] = 3;
    let put = |h: &mut Vec<u8>, at: usize, v: u64| h[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put(&mut header, 8, HEADER_LENGTH);
    put(&mut header, 16, dir.len() as u64);
    put(&mut header, 40, HEADER_LENGTH + dir.len() as u64);
    put(&mut header, 48, 0);
    put(&mut header, 56, HEADER_LENGTH + dir.len() as u64);
    put(&mut header, 64, data.len() as u64);
    header[97] = 1;
    header[98] = COMPRESSION_NONE;
    header[99] = TILE_TYPE_WEBP;
    header[100] = 0;
    header[101] = 2;
    [header, dir, data].concat()
}

struct Server {
    file: Mutex<(Vec<u8>, String)>,
    requests: AtomicUsize,
    /// Replace the file with version 2 after this many requests.
    change_after: Option<usize>,
}

impl Server {
    fn new(version: u8) -> Self {
        Self {
            file: Mutex::new((archive(version), format!("\"v{version}\""))),
            requests: AtomicUsize::new(0),
            change_after: None,
        }
    }
}

impl Transport for Server {
    fn probe(&self, _: &str) -> io::Result<RemoteInfo> {
        Err(io::Error::other("unused"))
    }

    fn fetch(&self, _: &str, _: Option<(u64, &str)>) -> io::Result<Fetched> {
        Err(io::Error::other("unused"))
    }

    fn fetch_range(
        &self,
        url: &str,
        start: u64,
        length: u64,
        etag: Option<&str>,
    ) -> io::Result<(RemoteInfo, Vec<u8>)> {
        assert_eq!(url, URL);
        let n = self.requests.fetch_add(1, Ordering::SeqCst);
        if self.change_after == Some(n) {
            *self.file.lock().unwrap() = (archive(2), "\"v2\"".into());
        }
        let (data, current) = &*self.file.lock().unwrap();
        if etag.is_some_and(|e| e != current) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "changed"));
        }
        let (start, end) = (start as usize, (start + length) as usize);
        Ok((
            RemoteInfo {
                length: Some(data.len() as u64),
                etag: Some(current.clone()),
                last_modified: Some("Fri, 11 Sep 2026 14:58:22 GMT".into()),
                accepts_ranges: true,
            },
            data[start..end.min(data.len())].to_vec(),
        ))
    }
}

fn run(store: &PackageStore, server: &Server) -> Result<TerrainOutcome, UpdateError> {
    run_with(store, server, &AtomicBool::new(false), &mut |_| {})
}

fn run_with(
    store: &PackageStore,
    server: &Server,
    cancelled: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<TerrainOutcome, UpdateError> {
    update_terrain(
        store,
        &ILLINOIS,
        AREA,
        URL,
        2,
        server,
        &options(),
        progress,
        cancelled,
    )
}

#[test]
fn downloads_the_area_and_serves_its_tiles() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let server = Server::new(1);
    let mut reports = Vec::new();
    let outcome = run_with(&store, &server, &AtomicBool::new(false), &mut |p| {
        reports.push(p)
    })
    .unwrap();
    let TerrainOutcome::Activated { terrain, replaced } = outcome else {
        panic!("expected a new terrain");
    };
    assert!(replaced.is_none());
    assert_eq!(terrain.record.tiles, 4);
    assert_eq!(terrain.record.max_zoom, 2);
    assert_eq!(terrain.record.source.etag.as_deref(), Some("\"v1\""));
    assert!(matches!(reports.last(), Some(Progress::Activating)));
    assert!(reports.iter().any(|p| matches!(
        p,
        Progress::Downloading { received, total: Some(total) } if received == total
    )));

    let tiles = TerrainTiles::open(&terrain.path).unwrap();
    assert_eq!(tiles.tile(2, 1, 1).unwrap(), Some(tile_bytes(2, 1, 1, 1)));
    assert_eq!(tiles.tile(1, 0, 0).unwrap(), Some(tile_bytes(1, 0, 0, 1)));
    // Outside the area, and past the edge of the zoom.
    assert_eq!(tiles.tile(2, 3, 3).unwrap(), None);
    assert_eq!(tiles.tile(1, 0, 9).unwrap(), None);
    assert_eq!(active_terrain(&store, "us-il").unwrap().unwrap(), terrain);

    // Header, root directory, and the tiles in one joined request.
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);
    assert!(matches!(
        run(&store, &server).unwrap(),
        TerrainOutcome::UpToDate(_)
    ));
}

#[test]
fn replaces_terrain_when_the_server_has_a_new_file() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let first = match run(&store, &Server::new(1)).unwrap() {
        TerrainOutcome::Activated { terrain, .. } => terrain,
        other => panic!("{other:?}"),
    };
    let TerrainOutcome::Activated { terrain, replaced } = run(&store, &Server::new(2)).unwrap()
    else {
        panic!("expected new terrain");
    };
    assert_eq!(replaced.as_deref(), Some(first.version.as_str()));
    let tiles = TerrainTiles::open(&terrain.path).unwrap();
    assert_eq!(tiles.tile(0, 0, 0).unwrap(), Some(tile_bytes(0, 0, 0, 2)));
    drop(tiles);
    assert_eq!(prune_terrain(&store, "us-il"), 0);
    assert!(!first.path.exists());
}

#[test]
fn a_file_changed_mid_download_is_not_mixed_in() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let mut server = Server::new(1);
    // Changes after the header and root were read, before the tiles.
    server.change_after = Some(2);
    let error = run(&store, &server).unwrap_err();
    assert!(matches!(error, UpdateError::Network(_)), "{error:?}");
    assert!(active_terrain(&store, "us-il").unwrap().is_none());
    // The next attempt starts again from the new file.
    let TerrainOutcome::Activated { terrain, .. } = run(&store, &Server::new(2)).unwrap() else {
        panic!("expected terrain");
    };
    assert_eq!(terrain.record.source.etag.as_deref(), Some("\"v2\""));
}

#[test]
fn cancelling_keeps_progress_for_next_time() {
    let dir = TempDir::new();
    let store = PackageStore::new(&dir.0);
    let server = Server::new(1);
    let cancelled = AtomicBool::new(false);
    let error = run_with(&store, &server, &cancelled, &mut |p| {
        if matches!(p, Progress::Downloading { .. }) {
            cancelled.store(true, Ordering::SeqCst);
        }
    })
    .unwrap_err();
    assert!(matches!(error, UpdateError::Cancelled));
    assert!(
        store
            .staging_dir("us-il/terrain")
            .join("staging.json")
            .exists()
    );
    assert!(matches!(
        run(&store, &server).unwrap(),
        TerrainOutcome::Activated { .. }
    ));
    assert!(
        !store
            .staging_dir("us-il/terrain")
            .join("staging.json")
            .exists()
    );
}

#[test]
fn tile_ranges_cover_the_area() {
    assert_eq!(tile_range(0, &AREA), (0, 0, 0, 0));
    assert_eq!(tile_range(2, &AREA), (0, 1, 1, 1));
    let illinois = Bounds {
        west: -91.516,
        south: 36.967,
        east: -87.493,
        north: 42.509,
    };
    let (x0, x1, y0, y1) = tile_range(12, &illinois);
    // 3901 tiles exist for this box in Mapterhorn's archive (8 November 2026).
    assert_eq!(u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1), 3901);
}
