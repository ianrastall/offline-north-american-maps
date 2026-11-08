//! Development tools for Offline Maps. Not part of the shipped application.
//!
//!   offline-maps-dev inspect <package.mbtiles> [--source-url U] [--etag E]
//!       [--last-modified L] [--downloaded-at T] [--record out.json]
//!   offline-maps-dev serve <package.mbtiles> [--port 8765]
//!
//! `serve` binds 127.0.0.1 only and stands in for the WebView2 request
//! interception the app will use: tiles come from the Rust core, every
//! response carries a same-origin CSP, and violations are counted.
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

mod server;

use offline_maps_core::package::{ILLINOIS, SourceInfo, verify_package};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("inspect") => inspect(&args[1..]),
        Some("serve") => serve(&args[1..]),
        _ => Err("usage: offline-maps-dev inspect|serve <package.mbtiles> [options]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

type Options = (PathBuf, HashMap<String, String>);

fn parse(args: &[String]) -> Result<Options, String> {
    let (path, rest) = args.split_first().ok_or("missing package path")?;
    let mut options = HashMap::new();
    let mut iter = rest.iter();
    while let Some(flag) = iter.next() {
        let name = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("unexpected argument {flag}"))?;
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        options.insert(name.to_owned(), value.clone());
    }
    Ok((PathBuf::from(path), options))
}

fn inspect(args: &[String]) -> Result<(), String> {
    let (path, options) = parse(args)?;
    let now = utc_now();
    let source = SourceInfo {
        url: options
            .get("source-url")
            .cloned()
            .unwrap_or_else(|| ILLINOIS.package_url.to_owned()),
        etag: options.get("etag").cloned(),
        last_modified: options.get("last-modified").cloned(),
    };
    let downloaded_at = options
        .get("downloaded-at")
        .cloned()
        .unwrap_or_else(|| now.clone());
    let record = verify_package(
        &path,
        &ILLINOIS,
        source,
        &downloaded_at,
        &now,
        &AtomicBool::new(false),
    )
    .map_err(|e| e.to_string())?;
    let json = record.to_json();
    match options.get("record") {
        Some(out) => {
            std::fs::write(out, &json).map_err(|e| format!("{out}: {e}"))?;
            println!("Verified; record written to {out}");
        }
        None => println!("{json}"),
    }
    Ok(())
}

fn serve(args: &[String]) -> Result<(), String> {
    let (path, options) = parse(args)?;
    let port: u16 = options
        .get("port")
        .map_or(Ok(8765), |p| p.parse())
        .map_err(|_| "invalid port")?;
    let root = project_root()?;
    server::run(&path, &root, port)
}

/// The workspace root, found from the executable's build location or the
/// current directory.
fn project_root() -> Result<PathBuf, String> {
    let candidates = [
        std::env::current_dir().ok(),
        Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join("assets/styles").is_dir() && dir.join("proof/renderer").is_dir())
        .map(|dir| dir.canonicalize().unwrap_or(dir))
        .ok_or_else(|| "run from the offline-maps project directory".into())
}

/// Current UTC time as RFC 3339 (seconds precision).
pub fn utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
