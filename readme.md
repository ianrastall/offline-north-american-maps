# Offline Maps

Jupiter desktop app for downloading road maps before an emergency and using them without internet access. Regions: the US states except Alaska, the District of Columbia, Puerto Rico and the US Virgin Islands, in any combination. Follows Jupiter style standard 1.1.

Status (8 November 2026): version 0.4.0 is installed on Ian's machine. It is a WinUI 3/C# app over a Rust core, with a Windows installer, a Start Menu entry and an uninstaller.

- **Map:** downloads Geofabrik's map package for each chosen region (Map > Regions) with resume, verification and atomic activation, and shows the installed regions as one map with MapLibre in WebView2, entirely from local files. Tiles on a border between installed regions are merged. Light and dark map styles show interstate, US and state route shields.
- **Terrain:** an optional download per region of Mapterhorn elevation tiles (USGS 3DEP and Copernicus GLO-30, to zoom 12, about 15 m per pixel), drawn as hillshading and contour lines in feet or metres. View > Terrain shading and View > Contour lines turn them on and off.
- **Downloads:** queued and run one at a time; Map > Update all regions checks every installed region.
- **Search:** finds places, streets, landmarks, transit and water offline, across all installed regions. An optional second download adds fuel stations, charging stations, house-number addresses and outdoor places (shelters, drinking water, campgrounds, public toilets), read from Geofabrik's raw extract; outdoor places are also drawn on the map (View > Outdoor places).
- **Saved places:** kept apart from map packages, so they survive map updates, app upgrades and uninstalling.
- **Not included:** routing.
- **Not yet verified:** a launch with networking disabled.

- [Project brief](docs/project-brief.md)
- [Sources and technical decisions](docs/sources-and-architecture.md)
- [Renderer proof and decision](docs/renderer-proof.md)
- [Implementation stages and verification](docs/validation.md)
- [Jupiter conformance](docs/jupiter-exceptions.md)

## Layout

| Path | Contents |
| --- | --- |
| `crates/offline-maps-core` | Rust domain core: region catalog, packages, downloads, store, tile merging, search, extras, terrain (PMTiles index reader), saved places |
| `crates/offline-maps-ffi` | C ABI facade (`offline_maps_ffi.dll`, ABI version 1) for the WinUI shell |
| `crates/offline-maps-dev` | Development command-line tools (not shipped) |
| `proto/offline_maps.proto` | Messages between the shell and the core |
| `src/OfflineMaps.Interop` | Managed bindings (`MapCore`) with owned handles |
| `src/OfflineMaps.App` | WinUI 3 app |
| `web/` | The map page hosted in WebView2 |
| `tests/OfflineMaps.Interop.Tests` | Managed/native boundary tests |
| `build/` | Release script, native build target, installer script |

## Build and release

Windows 11 x64; Rust 1.99.1, .NET SDK 10.0.114 and Inno Setup 6. Jupiter.UI must be at `../jupiter-ui`.

One-time prerequisites:

- MapLibre, maplibre-contour, sprites and glyphs (Node 24; pinned hashes): `cd proof/renderer; npm ci; node fetch-assets.mjs`.
- The signed WebView2 offline runtime installer at `third_party/webview2/MicrosoftEdgeWebView2RuntimeInstallerX64.exe` (the Evergreen Standalone x64 installer). It is not committed.

Release (checks, Rust and interop tests, publish, installer to `dist/`):

```powershell
pwsh build/build.ps1
```

Development checks:

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
dotnet test tests/OfflineMaps.Interop.Tests -c Release
dotnet build src/OfflineMaps.App -c Release
```

The app's data lives in `%LOCALAPPDATA%\Jupiter\OfflineMaps`:

- `settings.json`, the log and the WebView2 profile;
- `Maps\` for map packages, stations and addresses, and saved places.

For testing, these command-line options and environment variables change where data is kept and downloaded from:

- `--store DIR` and `--settings DIR` use other folders.
- `OFFLINE_MAPS_PACKAGE_URL` and `OFFLINE_MAPS_EXTRAS_URL` download from another server, such as the dev server below.

## Development tools

```powershell
node tools/build-styles.mjs
python tools/third-party-notices.py
cargo run --release -p offline-maps-dev -- inspect data/sources/illinois-shortbread-1.0.mbtiles
cargo run --release -p offline-maps-dev -- serve data/sources/illinois-shortbread-1.0.mbtiles [--extras FILE] [--source-pbf FILE]
cargo run --release -p offline-maps-dev -- probe
cargo run --release -p offline-maps-dev -- download data/store [--url URL] [--stop-after BYTES]
cargo run --release -p offline-maps-dev -- download-extras data/store [--url URL] [--stop-after BYTES]
cargo run --release -p offline-maps-dev -- search data/store "main st clinton" [--near LAT,LON]
cargo run --release -p offline-maps-dev -- extract data/sources/illinois-latest.osm.pbf data/illinois-extras.sqlite
```

- `probe` sends one HEAD request.
- `download` and `download-extras` without `--url` download from Geofabrik (about 363 MB each). The dev server serves local copies at `http://127.0.0.1:8765/source/package.mbtiles` and, with `--source-pbf`, at `/source/extract.osm.pbf`.
- The dev server's browser page is at http://127.0.0.1:8765/.
- Map data goes under `data/`, which is not committed.
