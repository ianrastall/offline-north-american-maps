# Offline Maps

Jupiter project for downloading road maps before an emergency and using them without internet access. Working application name: **Offline Maps**. First region: **Illinois**.

Status: project started on 27 October 2026. Product brief, source assessment and a tested Rust package-transfer foundation exist. On 8 November 2026 a [renderer proof](docs/renderer-proof.md) rendered Geofabrik's Illinois vector tiles entirely from local files with MapLibre; MapLibre in WebView2 is the selected map surface. The Rust core reads and verifies MBTiles packages and downloads them with resume, verification and atomic activation that keeps the previous map through any failure; derived light/dark styles show interstate, US and Illinois route shields. Offline search (places, streets, points of interest, transit, water) is built with each package, and saved places are stored apart from packages. The package lacks fuel and charging stations, so they are read from Geofabrik's raw Illinois extract into a small separate extras file. They are searchable ("gas", "ev charger") and shown on the map. Address search is not available. There is no app map viewer, routing engine or installed application yet.

- [Project brief](docs/project-brief.md)
- [Sources and technical decisions](docs/sources-and-architecture.md)
- [Renderer proof and decision](docs/renderer-proof.md)
- [Implementation stages and verification](docs/validation.md)
- [Jupiter conformance](docs/jupiter-exceptions.md)

## Development

Windows 11 x64, Rust 1.99.1 (pinned). From this directory:

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

Map page and development server (Node 24 for asset fetching and style builds):

```powershell
cd proof/renderer; npm ci; node fetch-assets.mjs; cd ../..
node tools/build-styles.mjs
cargo run --release -p offline-maps-dev -- inspect data/sources/illinois-shortbread-1.0.mbtiles
cargo run --release -p offline-maps-dev -- serve data/sources/illinois-shortbread-1.0.mbtiles
cargo run --release -p offline-maps-dev -- probe
cargo run --release -p offline-maps-dev -- download data/store [--url URL] [--stop-after BYTES]
cargo run --release -p offline-maps-dev -- search data/store "main st clinton" [--near LAT,LON]
cargo run --release -p offline-maps-dev -- download-extras data/store [--url URL] [--stop-after BYTES]
cargo run --release -p offline-maps-dev -- extract data/sources/illinois-latest.osm.pbf data/illinois-extras.sqlite
```

`probe` sends one HEAD request. `download` without `--url` downloads the 362 MB Illinois package from Geofabrik; with `--url http://127.0.0.1:8765/source/package.mbtiles` it uses the local dev server instead. `download-extras` downloads the 363 MB raw extract and keeps only the extracted stations. With `--source-pbf FILE`, the dev server serves a local copy at `/source/extract.osm.pbf`; with `--extras FILE`, it shows stations on the map.

The map page is at http://127.0.0.1:8765/ (`?theme=dark` for dark). Downloaded map data and assets go under `data/`, which is not committed.

The future WinUI 3/C# application must import Jupiter.UI and follow Jupiter style standard 1.1. Normal release builds must produce an installer with bundled prerequisites, a Start Menu entry, registered uninstall and upgrades preserving settings and downloaded maps. No GUI is delivered by this initial core milestone.
