# Sources and architecture

Checked 27 October 2026. Source pages can change; recheck before release.

## Provider decision

Google's Maps Static API supports satellite images, but its ordinary size is 640 × 640 logical pixels (scale 2 yields 1280 × 1280). The API returning image bytes does not grant general offline redistribution or bulk download rights. Google's terms section 3.2.3 restricts scraping, caching and bulk downloads; Map Tiles policies explicitly restrict offline use. These APIs are not the selected foundation for this offline road-map workflow.

- https://developers.google.com/maps/documentation/maps-static/start
- https://cloud.google.com/maps-platform/terms
- https://developers.google.com/maps/documentation/tile/policies

OpenStreetMap data is available under ODbL with attribution and applicable share-alike obligations. Geofabrik distributes regional extracts suitable for local processing. Its Illinois page lists the raw .osm.pbf extract and an experimental Shortbread vector tile package. The vector tile package is the selected map package and renders directly without local preparation; PBF is the source-processing fallback (for example, for search indexing or routing).

- Illinois: https://download.geofabrik.de/north-america/us/illinois.html
- Source data: https://download.geofabrik.de/north-america/us/illinois-latest.osm.pbf
- Extract details: https://download.geofabrik.de/technical.html
- License/attribution: https://www.openstreetmap.org/copyright

Do not implement a bulk downloader against tile.openstreetmap.org. Its public tile service prohibits bulk downloads and offline prefetching. Download regional datasets from a service that offers them for that purpose, then render locally.

- https://operations.osmfoundation.org/policies/tiles/

## Canonical architecture

- Rust core: package catalogue, bounded download streaming, validation, package persistence, local search index and map-domain queries.
- WinUI 3/C#: map workspace, MVVM, Windows pickers, accessibility, credentials if later needed, window state and Jupiter.UI appearance settings.
- Narrow versioned C ABI with protobuf commands/results and SafeHandle ownership. Add at the integration stage; it is not implemented yet.
- Renderer, selected 8 November 2026 after the [renderer proof](renderer-proof.md): MapLibre GL JS in WebView2 renders Geofabrik Shortbread MBTiles with bundled style (derived from VersaTiles, MIT), sprites (CC0) and Noto Sans glyphs (OFL). Static assets load through a WebView2 virtual host; tiles come from the Rust core through request interception. Recorded as a [Jupiter exception](jupiter-exceptions.md).
- Raw PBF is road/source data, not a renderable map package. Rendering conversion, local place search and a routing graph are distinct derived artifacts with separate validation and storage costs.

## Licensing obligations

- OpenStreetMap data (ODbL 1.0): show "© OpenStreetMap contributors" on the map and keep license details with packages. Distributing tiles or a derived database (for example committing them to a public repository or bundling them in a release) triggers share-alike obligations, so map data stays out of git and out of the installer; the app downloads it from Geofabrik.
- MapLibre GL JS: BSD-3-Clause. VersaTiles Style: MIT. VersaTiles sprites: CC0. Noto Sans: OFL 1.1. Ship their license texts with the app.
- Geofabrik labels the Shortbread package experimental. Its file is rebuilt about weekly; record ETag and Last-Modified per download.

## Package storage contract to implement

Keep mutable settings and packages outside the installation directory. Persist versioned metadata including region ID, source URL, license, source date, downloaded date, format, byte length and SHA-256. Download to staging storage with explicit size/disk limits. Verify source version when resuming: never append to a changed latest file. Validate format and renderer readability before atomically activating a version. Keep the last known working map until replacement passes checks. Never treat transfer completion as map readiness.

The initial core implements bounded streaming, cancellation checks, coordinate validation and a minimal package readiness model. HTTP timeouts, resume/ETag handling, hashes, durable storage, catalog retrieval and rendering remain to implement.
