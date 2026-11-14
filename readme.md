# Offline Maps

A Windows app for downloading road maps before an emergency and using them without internet access. Regions: the US states, the District of Columbia, Puerto Rico and the US Virgin Islands; the Canadian provinces and territories; and Mexico, as a whole or by state, in any combination. Everything runs on your computer; there is no account, server or tracking.

## Install

Download `OfflineMaps-<version>-win-x64-setup.exe` from [Releases](https://github.com/ianrastall/offline-maps/releases) and run it. It installs for the current user without administrator rights, adds Offline Maps to the Start Menu and registers an uninstaller. Requires Windows 11 24H2 or later, x64. Maps, saved places and settings (in `%LOCALAPPDATA%\Jupiter\OfflineMaps`, or the map folder chosen in Settings) are kept through upgrades and uninstalling.

Then open Map > Regions, choose regions and download them while you have a connection.

## Features

- **Overview:** a map of the whole world is built in (Natural Earth: coast, lakes, rivers, state, province and country borders, main highways, cities, country names), so the map shows every country from the first start and around the regions you install. Zoomed out, the overview is the whole map; closer in, each installed region's own map replaces it. Pointing at a region that can be downloaded and has no map installed highlights it; clicking it offers its download.
- **Map:** downloads the map package for each chosen region (Geofabrik's, or for Canada, Alaska and the Mexican states one made from Geofabrik's data and published in this repository's [`regions` release](https://github.com/ianrastall/offline-maps/releases/tag/regions)) with resume, verification and atomic activation, and shows the installed regions as one map with MapLibre in WebView2, entirely from local files. Tiles on a border between installed regions are merged. Light and dark map styles show interstate, US and state route shields.
- **Terrain:** an optional download per region of Mapterhorn elevation tiles (USGS 3DEP and Copernicus GLO-30, to zoom 12, about 15 m per pixel), drawn as hillshading and contour lines in feet or metres. View > Terrain shading and View > Contour lines turn them on and off.
- **Flood zones and public lands:** an optional download per region of FEMA flood hazard zones and USGS PAD-US public lands (coloured by public access), prebuilt by `tools/layers/build_overlays.py` and published in this repository's [`overlays` release](https://github.com/ianrastall/offline-maps/releases/tag/overlays). View > Flood zones and View > Public lands turn them on and off. US regions only.
- **Downloads and updates:** queued and run one at a time. Installed maps never expire. Map > Check for updates (or Check for updates in Map > Regions) asks the servers whether anything newer exists, without downloading, and lists what is newer with dates and sizes; nothing is downloaded until you choose. Geofabrik rebuilds its files daily, so a newer map usually adds only recent edits.
- **Search:** finds places, streets, landmarks, transit and water offline, across all installed regions, with or without accents ("queretaro" finds Querétaro) and with common English, Spanish and French street abbreviations (St, Ave, Av., Calz., Carr., boul., ch.). An optional second download adds fuel stations, charging stations, house-number addresses and outdoor places (shelters, drinking water, campgrounds, public toilets), read from Geofabrik's raw extract; outdoor places are also drawn on the map (View > Outdoor places).
- **Saved places:** kept apart from map packages, so they survive map updates, app upgrades and uninstalling.
- **Not included:** routing, and current conditions such as road closures or active flooding. Flood zones are FEMA's mapped hazard areas, not forecasts.

## Data and licences

The app's own code is in the public domain under [The Unlicense](LICENSE). Bundled components keep their own licences (MapLibre GL JS and maplibre-contour BSD-3-Clause, VersaTiles style MIT, sprites CC0, Noto Sans OFL, Rust crates MIT/Apache-2.0); see [THIRD-PARTY-NOTICES.txt](assets/notices/THIRD-PARTY-NOTICES.txt).

The built-in overview is made from [Natural Earth](https://www.naturalearthdata.com/) (public domain) by `tools/overview/build_overview.py`. Other map data is not part of the program. You download it yourself, under its own terms:

- Maps, stations and addresses: Geofabrik, © OpenStreetMap contributors, [ODbL 1.0](https://www.openstreetmap.org/copyright).
- Terrain: [Mapterhorn](https://mapterhorn.com/attribution) (USGS 3DEP, public domain; Copernicus GLO-30).
- Flood zones: FEMA National Flood Hazard Layer; public lands: USGS PAD-US. Both are US Government works in the public domain. The flood data's effective date is shown in Map > Regions.

Status (14 November 2026): version 0.12.0, with a built-in world overview and 100 regions: the US (Alaska included), Canada and Mexico by state. All 52 US regions' flood zones and public lands are published. A WinUI 3/C# app over a Rust core. On 11 November 2026 the installed app passed the offline acceptance test: with no network, it showed the Illinois map and found places, addresses, stations, outdoor places and saved places ([validation](docs/validation.md#offline-acceptance-test)).

Planned: region maps and processed stations and addresses published in this repository's releases (11 November 2026), and an Android app with GPS that receives maps from the desktop (10 November 2026). See the [North America and Android plan](docs/north-america-and-android.md). Proposed on 14 November 2026: regions for the whole world, filled in on demand over a world overview ([worldwide plan](docs/worldwide.md)).

- [Project brief](docs/project-brief.md)
- [North America and Android plan](docs/north-america-and-android.md)
- [Worldwide plan](docs/worldwide.md) (proposed 14 November 2026)
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
| `tools/regions-build/` | Map files for regions Geofabrik does not publish on their own (Mexican states, Canada, Alaska), published in the `regions` release |
| `tools/acceptance/` | Scripted check of the installed app (map, search, stations, addresses, saved places), with or without a network |
| `tools/layers/` | Overlay build (flood zones, public lands): Python with GDAL wheels and Planetiler; run by the maintainer, not part of the app |

## Build and release

Windows 11 x64; Rust 1.99.1, .NET SDK 10.0.114 and Inno Setup 6. The shared style library [Jupiter.UI](https://github.com/ianrastall/jupiter-ui) must be cloned beside this repository:

```powershell
git clone https://github.com/ianrastall/offline-maps
git clone https://github.com/ianrastall/jupiter-ui
cd offline-maps
```

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
- `Maps\` for map packages, stations and addresses, and saved places, unless another map folder is chosen in Settings > Map folder > Change. Changing it offers to move the maps and saved places there (renamed on the same drive, copied and then removed on another; cancelling or a failure leaves them where they were) or to use the new folder as it is. Only the app's own region folders and saved-places files are moved; other files in either folder are left alone.

For testing, these command-line options and environment variables change where data is kept and downloaded from:

- `--store DIR` and `--settings DIR` use other folders.
- `OFFLINE_MAPS_PACKAGE_URL` and `OFFLINE_MAPS_EXTRAS_URL` download from another server, such as the dev server below.
- `OFFLINE_MAPS_OVERLAYS_URL` downloads flood zones and public lands from another address (the folder holding `<region>-overlays.mbtiles`).

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
cargo run --release -p offline-maps-dev -- terrain-size <west> <south> <east> <north>
python -I tools/extract-boxes.py data/catalog/index-v1.json data/catalog/extract-boxes.txt
```

- `probe` sends one HEAD request. `terrain-size` and `tools/extract-boxes.py` measure inputs of `tools/make-regions.py` (terrain bytes from Mapterhorn's index; each raw extract's header box from its first 64 KB).
- `download` and `download-extras` without `--url` download from Geofabrik (about 363 MB each). The dev server serves local copies at `http://127.0.0.1:8765/source/package.mbtiles` and, with `--source-pbf`, at `/source/extract.osm.pbf`.
- The dev server's browser page is at http://127.0.0.1:8765/.
- Map data goes under `data/`, which is not committed.
