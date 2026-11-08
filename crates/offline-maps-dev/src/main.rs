//! Development tools for Offline Maps. Not part of the shipped application.
//!
//!   offline-maps-dev inspect <package.mbtiles> [--source-url U] [--etag E]
//!       [--last-modified L] [--downloaded-at T] [--record out.json]
//!   offline-maps-dev serve <package.mbtiles> [--port 8765]
//!   offline-maps-dev download <store-dir> [--url U] [--stop-after BYTES]
//!   offline-maps-dev probe [url]      (HEAD request only; downloads nothing)
//!   offline-maps-dev index <package.mbtiles> <search.sqlite>
//!   offline-maps-dev search <store-dir> <query> [--near LAT,LON] [--limit N]
//!       (builds the active package's search index first if it is missing)
//!
//! `serve` binds 127.0.0.1 only and stands in for the WebView2 request
//! interception the app will use: tiles come from the Rust core, every
//! response carries a same-origin CSP, and violations are counted.
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

mod server;

use offline_maps_core::package::{ILLINOIS, SourceInfo, verify_package};

fn probe(url: &str) -> Result<(), String> {
    use offline_maps_core::download::{HttpTransport, Transport};
    let info = HttpTransport::new()
        .probe(url)
        .map_err(|e| format!("{url}: {e}"))?;
    println!("{url}\n{info:#?}\nstrong ETag: {:?}", info.strong_etag());
    Ok(())
}

fn package_sha(path: &std::path::Path) -> Result<String, String> {
    offline_maps_core::package::sha256_file(path, &AtomicBool::new(false))
        .map(|(sha, _)| sha)
        .map_err(|e| e.to_string())
}

fn index(args: &[String]) -> Result<(), String> {
    use offline_maps_core::mbtiles::TilePackage;
    let [package, out] = args else {
        return Err("usage: index <package.mbtiles> <search.sqlite>".into());
    };
    let started = std::time::Instant::now();
    let sha = package_sha(package.as_ref())?;
    let package = TilePackage::open(package.as_ref()).map_err(|e| e.to_string())?;
    let mut last = 0;
    let stats = offline_maps_core::search::build_index(
        &package,
        &sha,
        out.as_ref(),
        &AtomicBool::new(false),
        &mut |done, total| {
            let percent = done * 100 / total.max(1);
            if percent / 10 != last / 10 {
                last = percent;
                println!("  {percent}% ({:.1} s)", started.elapsed().as_secs_f64());
            }
        },
    )
    .map_err(|e| e.to_string())?;
    let size = std::fs::metadata(out).map_err(|e| e.to_string())?.len();
    println!(
        "{stats:?}\n{} entries, {:.1} MB, {:.1} s",
        stats.entries(),
        size as f64 / 1e6,
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn search(args: &[String]) -> Result<(), String> {
    use offline_maps_core::Coordinates;
    use offline_maps_core::store::PackageStore;
    let [store_dir, query, rest @ ..] = args else {
        return Err("usage: search <store-dir> <query> [--near LAT,LON] [--limit N]".into());
    };
    let (_, options) = parse(
        &[String::new()]
            .iter()
            .chain(rest)
            .cloned()
            .collect::<Vec<_>>(),
    )?;
    let near = options
        .get("near")
        .map(|v| {
            let (lat, lon) = v.split_once(',').ok_or("--near LAT,LON")?;
            Coordinates::new(
                lat.parse().map_err(|_| "lat")?,
                lon.parse().map_err(|_| "lon")?,
            )
            .ok_or("invalid coordinates")
        })
        .transpose()?;
    let limit = options
        .get("limit")
        .map_or(Ok(10), |v| v.parse())
        .map_err(|_| "--limit")?;
    let active = PackageStore::new(store_dir)
        .active(ILLINOIS.id)
        .map_err(|e| e.to_string())?
        .ok_or("the store has no active Illinois package")?;
    let opening = std::time::Instant::now();
    let built = !active.search_index_path().exists();
    let index =
        offline_maps_core::search::open_or_build(&active, &AtomicBool::new(false), &mut |_, _| {})
            .map_err(|e| e.to_string())?;
    if built {
        println!(
            "Built search index for {} in {:.1} s",
            active.version,
            opening.elapsed().as_secs_f64()
        );
    }
    let started = std::time::Instant::now();
    let results = index
        .search(query, near, limit)
        .map_err(|e| e.to_string())?;
    println!(
        "{query:?}: {} results in {:.1} ms",
        results.len(),
        started.elapsed().as_secs_f64() * 1000.0
    );
    for r in results {
        println!(
            "  {:<40} {:<8} {:<16} {:<22} {:.4},{:.4}{}",
            match &r.reference {
                Some(reference) => format!("{} [{}]", r.name, reference.replace('\n', "/")),
                None => r.name.clone(),
            },
            format!("{:?}", r.kind).to_lowercase(),
            r.category,
            r.near.map(|n| format!("near {n}")).unwrap_or_default(),
            r.latitude,
            r.longitude,
            r.distance_km
                .map(|d| format!("  {d:.1} km"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("inspect") => inspect(&args[1..]),
        Some("serve") => serve(&args[1..]),
        Some("download") => download(&args[1..]),
        Some("probe") => probe(args.get(1).map_or(ILLINOIS.package_url, String::as_str)),
        Some("index") => index(&args[1..]),
        Some("search") => search(&args[1..]),
        _ => Err("usage: offline-maps-dev inspect|serve <package.mbtiles> | download <store-dir> [options]".into()),
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
    let now = offline_maps_core::time::utc_now();
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

/// Updates the Illinois package in `store-dir`. `--stop-after` cancels once
/// that many bytes have arrived, to exercise resume.
fn download(args: &[String]) -> Result<(), String> {
    use offline_maps_core::download::{
        HttpTransport, Progress, UpdateOptions, UpdateOutcome, update_region,
    };
    use offline_maps_core::package::Region;
    use offline_maps_core::store::PackageStore;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    let (store_dir, options) = parse(args)?;
    let region = Region {
        package_url: match options.get("url") {
            Some(url) => Box::leak(url.clone().into_boxed_str()),
            None => ILLINOIS.package_url,
        },
        ..ILLINOIS
    };
    let stop_after: Option<u64> = options
        .get("stop-after")
        .map(|v| v.parse().map_err(|_| "invalid --stop-after"))
        .transpose()?;
    let store = PackageStore::new(store_dir);
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();
    let mut last_percent = None;
    let mut first_received = None;
    let mut progress = |p: Progress| match p {
        Progress::Downloading { received, total } => {
            first_received.get_or_insert(received);
            if stop_after.is_some_and(|limit| received >= limit) {
                cancelled.store(true, Ordering::Relaxed);
            }
            let percent = total.map(|t| received * 100 / t.max(1));
            if percent.is_some_and(|p| p % 10 == 0) && percent != last_percent {
                last_percent = percent;
                println!(
                    "  {} / {} MB",
                    received / 1_000_000,
                    total.map_or("?".into(), |t| (t / 1_000_000).to_string())
                );
            }
        }
        Progress::Indexing { done, total } => {
            if done == total {
                println!(
                    "Indexed {total} tiles ({:.1} s)",
                    started.elapsed().as_secs_f64()
                );
            }
        }
        other => println!("{other:?} ({:.1} s)", started.elapsed().as_secs_f64()),
    };
    let result = update_region(
        &store,
        &region,
        &HttpTransport::new(),
        &UpdateOptions::default(),
        &mut progress,
        &cancelled,
    );
    if let Some(start) = first_received {
        println!("First progress at byte {start}");
    }
    let elapsed = started.elapsed().as_secs_f64();
    match result.map_err(|e| e.to_string())? {
        UpdateOutcome::UpToDate(active) => {
            println!("Up to date: {} ({elapsed:.1} s)", active.version)
        }
        UpdateOutcome::Activated {
            package,
            replaced,
            prune_pending,
        } => println!(
            "Activated {} ({} bytes, sha256 {}); replaced {:?}; prune pending {prune_pending} ({elapsed:.1} s)",
            package.version, package.record.byte_length, package.record.sha256, replaced
        ),
    }
    Ok(())
}
