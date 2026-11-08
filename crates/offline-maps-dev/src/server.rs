//! Minimal loopback HTTP/1.1 server for the renderer page. One request per
//! connection; no keep-alive, no TLS. Development use only.
//!
//! `/source/package.mbtiles` also serves the package file itself with HEAD,
//! ETag and single-range (`bytes=N-`) support, so the downloader can be
//! exercised end to end without the internet.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use offline_maps_core::mbtiles::{TileEncoding, TilePackage};

const CSP: &str = "default-src 'self'; script-src 'self'; worker-src 'self' blob:; \
    img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; \
    font-src 'self'; report-uri /csp-report";

#[derive(Default)]
struct Stats {
    requests: u64,
    tiles: u64,
    empty_tiles: u64,
    tile_bytes: u64,
    not_found: Vec<String>,
    missing_glyph_ranges: Vec<String>,
    violations: Vec<String>,
}

struct State {
    package: Mutex<TilePackage>,
    package_path: PathBuf,
    metadata_json: String,
    mounts: Vec<(&'static str, PathBuf)>,
    stats: Mutex<Stats>,
}

struct Response {
    status: &'static str,
    content_type: &'static str,
    gzip: bool,
    body: Vec<u8>,
}

impl Response {
    fn new(status: &'static str, content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type,
            gzip: false,
            body: body.into(),
        }
    }
    fn text(status: &'static str, body: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", body)
    }
}

pub fn run(package_path: &Path, root: &Path, port: u16) -> Result<(), String> {
    let package = TilePackage::open(package_path).map_err(|e| e.to_string())?;
    let metadata_json = format!(
        "{{\"metadata\":{},\"tiles\":\"/tiles/{{z}}/{{x}}/{{y}}.pbf\"}}",
        json_of(package.metadata())
    );
    let state = Arc::new(State {
        package: Mutex::new(package),
        package_path: package_path.to_owned(),
        metadata_json,
        mounts: vec![
            (
                "/maplibre/",
                root.join("proof/renderer/node_modules/maplibre-gl/dist"),
            ),
            ("/styles/", root.join("assets/styles")),
            ("/sprites/", root.join("data/renderer-assets/sprites")),
            ("/glyphs/", root.join("data/renderer-assets/glyphs")),
            ("/", root.join("proof/renderer/public")),
        ],
        stats: Mutex::default(),
    });
    let listener =
        TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
    println!(
        "Renderer proof: http://127.0.0.1:{port}/ ({})",
        package_path.display()
    );
    for stream in listener.incoming().flatten() {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            if let Err(error) = handle(stream, &state) {
                eprintln!("request failed: {error}");
            }
        });
    }
    Ok(())
}

fn json_of(metadata: &offline_maps_core::mbtiles::TileMetadata) -> String {
    serde_json::to_string(metadata).expect("metadata serializes")
}

fn handle(stream: TcpStream, state: &State) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut content_length = 0_usize;
    let mut range = None;
    let mut if_range = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        header_bytes += n;
        if n == 0 || line == "\r\n" || header_bytes > 16 * 1024 {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => content_length = value.parse().unwrap_or(0),
                "range" => range = Some(value.to_owned()),
                "if-range" => if_range = Some(value.to_owned()),
                _ => {}
            }
        }
    }
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");
    state.stats.lock().unwrap().requests += 1;

    if path == "/source/package.mbtiles" && (method == "GET" || method == "HEAD") {
        return serve_source(stream, state, method == "HEAD", range, if_range);
    }
    let response = if method == "POST" && path == "/csp-report" {
        let mut body = vec![0; content_length.min(64 * 1024)];
        reader.read_exact(&mut body)?;
        let report = String::from_utf8_lossy(&body).into_owned();
        eprintln!("CSP violation: {report}");
        state.stats.lock().unwrap().violations.push(report);
        Response::text("204 No Content", "")
    } else if method != "GET" {
        Response::text("405 Method Not Allowed", "GET only")
    } else {
        route(path, state)
    };
    write(stream, &response)
}

fn route(path: &str, state: &State) -> Response {
    if let Some(rest) = path.strip_prefix("/tiles/") {
        return tile(rest, state);
    }
    if let Some(rest) = path.strip_prefix("/glyphs/") {
        return glyphs(rest, state);
    }
    match path {
        "/proof/metadata" => {
            return Response::new("200 OK", "application/json", state.metadata_json.clone());
        }
        "/proof/stats" => {
            let s = state.stats.lock().unwrap();
            let list = |v: &[String]| {
                let mut unique: Vec<&String> = v.iter().collect();
                unique.sort();
                unique.dedup();
                format!("{unique:?}")
            };
            let body = format!(
                "{{\"requests\":{},\"tiles\":{},\"emptyTiles\":{},\"tileBytes\":{},\"violations\":{},\"notFound\":{},\"missingGlyphRanges\":{}}}",
                s.requests,
                s.tiles,
                s.empty_tiles,
                s.tile_bytes,
                s.violations.len(),
                list(&s.not_found),
                list(&s.missing_glyph_ranges)
            );
            return Response::new("200 OK", "application/json", body);
        }
        _ => {}
    }
    static_file(path, state)
}

fn tile(rest: &str, state: &State) -> Response {
    let parsed = rest
        .strip_suffix(".pbf")
        .map(|r| r.split('/').collect::<Vec<_>>())
        .and_then(|p| match p[..] {
            [z, x, y] => Some((z.parse().ok()?, x.parse().ok()?, y.parse().ok()?)),
            _ => None,
        });
    let Some((z, x, y)) = parsed else {
        return Response::text("400 Bad Request", "bad tile path");
    };
    let result = state.package.lock().unwrap().tile(z, x, y);
    let mut stats = state.stats.lock().unwrap();
    stats.tiles += 1;
    match result {
        Ok(Some(tile)) => {
            stats.tile_bytes += tile.data.len() as u64;
            Response {
                status: "200 OK",
                content_type: "application/x-protobuf",
                gzip: tile.encoding == TileEncoding::Gzip,
                body: tile.data,
            }
        }
        Ok(None) => {
            stats.empty_tiles += 1;
            Response::text("204 No Content", "")
        }
        Err(error) => Response::text("400 Bad Request", &error.to_string()),
    }
}

/// Serves `/{fontstack}/{range}.pbf`, using the first font in the stack that
/// has the range. A range not bundled is answered with an empty glyph set so
/// the renderer omits those characters instead of failing the label.
fn glyphs(rest: &str, state: &State) -> Response {
    let base = &state
        .mounts
        .iter()
        .find(|(p, _)| *p == "/glyphs/")
        .unwrap()
        .1;
    let Some((stack, file)) = rest.split_once('/') else {
        return Response::text("400 Bad Request", "bad glyph path");
    };
    let valid_range = file
        .strip_suffix(".pbf")
        .and_then(|r| r.split_once('-'))
        .is_some_and(|(a, b)| a.parse::<u32>().is_ok() && b.parse::<u32>().is_ok());
    if !valid_range {
        return Response::text("400 Bad Request", "bad glyph range");
    }
    let stack = stack.replace("%20", " ").replace("%2C", ",");
    for font in stack.split(',').map(str::trim) {
        if !font.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        if let Ok(body) = std::fs::read(base.join(font).join(file)) {
            return Response::new("200 OK", "application/x-protobuf", body);
        }
    }
    state
        .stats
        .lock()
        .unwrap()
        .missing_glyph_ranges
        .push(format!("{stack}/{file}"));
    Response::new("200 OK", "application/x-protobuf", Vec::new())
}

fn static_file(path: &str, state: &State) -> Response {
    let Some((prefix, base)) = state.mounts.iter().find(|(p, _)| path.starts_with(p)) else {
        return Response::text("404 Not Found", "not found");
    };
    let relative = match &path[prefix.len()..] {
        "" => "index.html",
        r => r,
    };
    let relative = PathBuf::from(relative.replace("%20", " "));
    let safe = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    let file = base.join(&relative);
    match (safe, std::fs::read(&file)) {
        (true, Ok(body)) => Response::new("200 OK", content_type(&file), body),
        _ => {
            state.stats.lock().unwrap().not_found.push(path.to_owned());
            Response::text("404 Not Found", "not found")
        }
    }
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript",
        "css" => "text/css",
        "json" | "map" => "application/json",
        "png" => "image/png",
        "pbf" => "application/x-protobuf",
        _ => "application/octet-stream",
    }
}

fn write(mut stream: TcpStream, response: &Response) -> std::io::Result<()> {
    let encoding = if response.gzip {
        "Content-Encoding: gzip\r\n"
    } else {
        ""
    };
    write!(
        stream,
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{encoding}\
         Content-Security-Policy: {CSP}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}

/// Streams the package file. The ETag is derived from size and modification
/// time; a `Range: bytes=N-` request is honoured only with a matching If-Range.
fn serve_source(
    mut stream: TcpStream,
    state: &State,
    head: bool,
    range: Option<String>,
    if_range: Option<String>,
) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom};
    let mut file = std::fs::File::open(&state.package_path)?;
    let meta = file.metadata()?;
    let length = meta.len();
    let modified = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let etag = format!("\"{length:x}-{modified:x}\"");
    let start = range
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|r| r.parse::<u64>().ok())
        .filter(|s| *s < length && if_range.as_deref() == Some(etag.as_str()));
    let mut headers = match start {
        Some(s) => format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{}/{length}\r\nContent-Length: {}\r\n",
            length - 1,
            length - s
        ),
        None => format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n"),
    };
    headers += &format!(
        "Content-Type: application/octet-stream\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(headers.as_bytes())?;
    if !head {
        file.seek(SeekFrom::Start(start.unwrap_or(0)))?;
        std::io::copy(&mut file, &mut stream)?;
    }
    stream.flush()
}
