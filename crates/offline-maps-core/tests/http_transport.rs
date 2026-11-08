//! Exercises the real HTTP transport against a loopback server that supports
//! HEAD, Range/If-Range and can drop a connection partway through a body.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use offline_maps_core::download::{
    Fetched, HttpTransport, Transport, UpdateError, UpdateOptions, UpdateOutcome, update_region,
};
use offline_maps_core::package::{ILLINOIS, Region};
use offline_maps_core::store::PackageStore;

struct Served {
    data: Vec<u8>,
    etag: String,
    /// Close the connection after this many body bytes on the next GET.
    drop_after: Option<usize>,
    requests: Vec<String>,
}

fn serve(state: Arc<Mutex<Served>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            let mut range = None;
            let mut if_range = None;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if request.is_empty() {
                    request = line.trim().to_owned();
                } else if let Some((name, value)) = line.split_once(':') {
                    let value = value.trim().to_owned();
                    match name.to_ascii_lowercase().as_str() {
                        "range" => {
                            range = value
                                .strip_prefix("bytes=")
                                .and_then(|r| r.strip_suffix('-'))
                                .and_then(|r| r.parse::<usize>().ok())
                        }
                        "if-range" => if_range = Some(value),
                        _ => {}
                    }
                }
            }
            let mut s = state.lock().unwrap();
            s.requests
                .push(format!("{request} range={range:?} if-range={if_range:?}"));
            let head = request.starts_with("HEAD");
            let len = s.data.len();
            let partial = range.filter(|_| if_range.as_deref() == Some(s.etag.as_str()));
            let (status, start) = match partial {
                Some(start) => ("206 Partial Content", start),
                None => ("200 OK", 0),
            };
            let mut out = stream;
            let mut headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: {}\r\nLast-Modified: Sun, 08 Nov 2026 04:12:34 GMT\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
                len - start,
                s.etag
            );
            if partial.is_some() {
                headers += &format!("Content-Range: bytes {start}-{}/{len}\r\n", len - 1);
            }
            let _ = out.write_all(format!("{headers}\r\n").as_bytes());
            if !head {
                let end = match s.drop_after.take() {
                    Some(n) => (start + n).min(len),
                    None => len,
                };
                let _ = out.write_all(&s.data[start..end]);
            }
        }
    });
    format!("http://{addr}/illinois.mbtiles")
}

fn package_bytes(marker: &[u8]) -> Vec<u8> {
    let path = std::env::temp_dir().join(format!(
        "offline-maps-http-{}-{}.mbtiles",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE metadata (name TEXT, value TEXT);
             CREATE TABLE tiles (zoom_level INTEGER, tile_column INTEGER, tile_row INTEGER, tile_data BLOB);",
        )
        .unwrap();
        let layers = r#"{"vector_layers":[{"id":"streets"},{"id":"street_labels"},{"id":"place_labels"},{"id":"water_polygons"},{"id":"land"},{"id":"boundaries"}]}"#;
        for (k, v) in [
            ("format", "pbf"),
            ("minzoom", "0"),
            ("maxzoom", "0"),
            ("bounds", "-91.5,36.9,-87.4,42.5"),
            ("json", layers),
        ] {
            db.execute("INSERT INTO metadata VALUES (?1, ?2)", (k, v))
                .unwrap();
        }
        // Enough data to span many reads.
        let tile: Vec<u8> = marker.iter().copied().cycle().take(300_000).collect();
        db.execute("INSERT INTO tiles VALUES (0, 0, 0, ?1)", [tile])
            .unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    bytes
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn region(url: String) -> Region {
    Region {
        package_url: Box::leak(url.into_boxed_str()),
        ..ILLINOIS
    }
}

fn options() -> UpdateOptions {
    UpdateOptions {
        available_space: |_| Ok(u64::MAX),
        ..UpdateOptions::default()
    }
}

#[test]
fn probe_full_and_ranged_fetches() {
    let data = package_bytes(b"abc");
    let state = Arc::new(Mutex::new(Served {
        data: data.clone(),
        etag: "\"e1\"".into(),
        drop_after: None,
        requests: Vec::new(),
    }));
    let url = serve(state.clone());
    let http = HttpTransport::new();

    let info = http.probe(&url).unwrap();
    assert_eq!(info.length, Some(data.len() as u64));
    assert_eq!(info.strong_etag(), Some("\"e1\""));
    assert!(info.accepts_ranges);

    let Fetched::Partial {
        start,
        info,
        mut body,
    } = http.fetch(&url, Some((1000, "\"e1\""))).unwrap()
    else {
        panic!("expected 206")
    };
    assert_eq!((start, info.length), (1000, Some(data.len() as u64)));
    let mut tail = Vec::new();
    body.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, data[1000..]);

    // A stale validator gets the whole (changed) file.
    assert!(matches!(
        http.fetch(&url, Some((1000, "\"old\""))).unwrap(),
        Fetched::Full { .. }
    ));
}

#[test]
fn dropped_connection_then_resume_over_http() {
    let data = package_bytes(b"xyz");
    let state = Arc::new(Mutex::new(Served {
        data: data.clone(),
        etag: "\"e1\"".into(),
        drop_after: Some(100_000),
        requests: Vec::new(),
    }));
    let region = region(serve(state.clone()));
    let dir = TempDir(
        std::env::temp_dir().join(format!("offline-maps-http-store-{}", std::process::id())),
    );
    let store = PackageStore::new(&dir.0);
    let http = HttpTransport::new();
    let cancelled = AtomicBool::new(false);

    let first = update_region(&store, &region, &http, &options(), &mut |_| {}, &cancelled);
    assert!(matches!(first, Err(UpdateError::Network(_))), "{first:?}");
    assert_eq!(store.read_staging("us-il").unwrap().1, 100_000);

    let second =
        update_region(&store, &region, &http, &options(), &mut |_| {}, &cancelled).unwrap();
    let UpdateOutcome::Activated { package, .. } = second else {
        panic!("expected activation")
    };
    assert_eq!(std::fs::read(&package.path).unwrap(), data);
    let requests = state.lock().unwrap().requests.clone();
    assert!(
        requests
            .last()
            .unwrap()
            .contains("range=Some(100000) if-range=Some(\"\\\"e1\\\"\")"),
        "{requests:#?}"
    );

    // Unchanged on the server: no further GET.
    let third = update_region(&store, &region, &http, &options(), &mut |_| {}, &cancelled).unwrap();
    assert!(matches!(third, UpdateOutcome::UpToDate(_)));
    assert_eq!(
        state
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r.starts_with("GET"))
            .count(),
        2
    );
}
