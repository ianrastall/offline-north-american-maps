# Implementation and validation

## Stage 1: project and core foundations

Implemented: Illinois source references, attribution constants, finite/range-checked coordinates, package readiness states and bounded streaming to caller-owned staging storage. Unit tests cover multibuffer transfers, unknown lengths, truncation, empty bodies, size limits, cancellation, write failures and readiness.

This is not an HTTP download or an offline map demonstration. The caller must supply a response reader and staging writer and validate package format separately.

Verified on Windows x64 on 27 October 2026 with Rust 1.99.1: all 8 unit tests passed; Clippy passed with warnings denied. Formatting checked with rustfmt. No dataset download or installed-app verification has been performed.

## Stage 2: renderer proof

Completed 8 November 2026; see [renderer proof](renderer-proof.md). Geofabrik's Illinois Shortbread MBTiles (362.6 MB, ODbL, zoom 0–14, 81,302 tiles) rendered in MapLibre GL JS 6.16.0 with a bundled VersaTiles 7.0.1 style, sprites and Noto Sans glyphs. All requests stayed on the loopback origin under a same-origin CSP with zero violations and zero map errors, from statewide zoom 7 to overzoomed zoom 17, in light and dark styles. No preparation step is needed: the package is directly renderable. Decision: MapLibre in WebView2 for the map surface, recorded as a Jupiter exception. Finding: the stock style hides interstate/US/state route numbers at regional zooms although the tiles contain them; Offline Maps needs its own derived style.

Not covered: WebView2 itself, Rust-served tiles, a network-adapter-off launch and long-session memory.

## Stage 3: map style, Rust-served tiles and package record

Completed 8 November 2026.

- **Rust core.** `mbtiles` opens a package read-only, validates its metadata (pbf format, zoom range, bounds, declared vector layers), serves tiles by XYZ address with gzip passthrough, and checks SQLite integrity and tile presence across declared zooms. `package` streams SHA-256 with cancellation, checks Illinois coverage and the Shortbread layers the style needs, and produces a versioned JSON `PackageRecord` keeping source URL, ETag, Last-Modified (data-date proxy), download time, verification time, size, hash, attribution and per-zoom tile counts. Sixteen unit tests cover row flipping, encoding detection, out-of-range tiles, malformed metadata, non-packages, missing or extra zooms, coverage, layer and schema rejection, hashing and record round trips.
- **Real package.** `offline-maps-dev inspect` verified the 8 November Illinois package in 2.1 s: SHA-256 `17c12c95…227cb9` (matches the download), 362,586,112 bytes, 81,302 tiles, integrity ok.
- **Dev server.** `offline-maps-dev serve` (std only, loopback, same-origin CSP) replaces the Node proof server and serves tiles through the core, standing in for WebView2 request interception. Unbundled glyph ranges return an empty glyph set (counted), path traversal and out-of-range tiles are refused, absent tiles return 204.
- **Styles.** `tools/build-styles.mjs` derives `assets/styles/offline-maps-{light,dark}.json` from VersaTiles 7.0.1 (MIT notice in `assets/styles/NOTICE.md`): same-origin paths only (the build fails on any remaining URL scheme), Mercator, OSM/Geofabrik attribution, and route shields drawn by the page — interstates from zoom 10, US/Illinois routes on trunk/primary roads from zoom 12, secondary routes from zoom 13. These are the zooms at which `tools/scan_tiles.py` found refs in the package; no street_labels exist below zoom 10. Shield images are registered when the style loads; registering them only on demand left some shields as bare text.
- **Glyphs.** The package uses 269 distinct characters in 25 glyph ranges, two of which (emoji, enclosed alphanumerics) Noto Sans does not cover. `assets/glyph-ranges.json` bundles those 23 ranges plus a Latin/Greek/Cyrillic/punctuation baseline: 2 fonts × 23 ranges, 7.0 MB instead of 77 MB.

Verified in the desktop browser pane: interstate shields at zoom 10 (I-74, I-72), US 51, US 51 Business, IL 10 and IL 54 at zoom 12, Chicago at zoom 11 dark (I-90, I-94, I-55, I-290, I-294, US 41), Chinatown at zoom 16, plus a sweep of Springfield, Carbondale, Champaign, western Illinois, the Loop, Quincy and Rockford. 0 map errors, 0 CSP violations, 0 missing glyph ranges, 105 tiles served by Rust; the only 404 was the browser's favicon request. The pane was hidden for much of this, so frame timings were throttled and are not reported.

Not covered: WebView2 hosting, a network-adapter-off launch, a scan of the package during download verification (glyph coverage is currently checked by the separate Python scan), and keyboard/accessibility of the map surface.

## Stage 4: download, staged storage and activation

Completed 8 November 2026.

- **Transport.** `HttpTransport` uses ureq 3.5.1 with native-tls (SChannel) and the Windows certificate store; no CA bundle is shipped. Finite timeouts: 20 s resolve and connect, 60 s to first response, 60 s per read, so a stalled connection fails instead of hanging. Every request sends `Cache-Control: no-cache` (see the Geofabrik finding below). Resumes use `Range: bytes=N-` with `If-Range: <strong ETag>`, and 206 responses must carry a consistent `Content-Range`.
- **Store.** `PackageStore` keeps `<root>/<region>/active.json`, `versions/<v>/{package.mbtiles,record.json}` and `staging/{package.part,download.json,lock}`. A version is complete before `active.json` names it, and `active.json` is replaced through a flushed temporary file and a single rename, so a crash leaves the old or the new package active, never a partial one. An OS file lock prevents concurrent updates. Version names are validated so a corrupted pointer cannot address outside the store.
- **Update flow.** `update_region` probes the server; reports up to date when the ETag and size match the active package or the server's copy is older; enforces a size cap (2 GiB default) and free space (download plus 256 MiB); resumes the staging file only when the URL, strong ETag and total size all still match, otherwise restarts; streams with progress and cancellation; verifies with `verify_package`; refuses to activate a download older than the active package; installs, activates and prunes old versions (best effort, so a version still open in the renderer is removed later). Failed verification discards the download; cancellation and network failures keep it for resume; the active package is untouched in every failure path.
- **Tests.** 32 unit tests, including ten update-flow tests with a fake server: first download, up-to-date detection, resume after a dropped transfer, restart when the file changes, restart when the server ignores ranges, cancellation, invalid download, replacement and pruning, size/disk/lock refusals, and never replacing with older data. Two integration tests drive the real transport over loopback HTTP, including a dropped connection followed by a ranged resume.
- **End to end (local).** `offline-maps-dev download` against the dev server's `/source/package.mbtiles` route with the real 362.6 MB package: cancelled at 150 MB (staging kept, 150,003,712 bytes); the next run resumed at that offset, verified and activated in 5.1 s with SHA-256 `17c12c95…227cb9` matching the original download; a third run reported up to date with no GET.
- **Live Geofabrik checks.** TLS through SChannel works; 404 and an expired certificate fail with clear errors.
- **Live download (Geofabrik, 07:32–07:34 UTC).** `offline-maps-dev download data/store-live`: cancelled at 100 MB (staging 100,004,067 bytes with ETag `"159ca000-65fbc0e0a1d71"` and Last-Modified 8 Nov 04:12:34); the next run resumed at that offset via a 206 response, downloaded the remaining 262.6 MB and verified and activated the package in 43.8 s (about 6 MB/s); SHA-256 `17c12c95…227cb9` matches the curl download. A third run reported up to date in 0.5 s with no GET.

**Geofabrik caching finding.** Geofabrik's download proxy (`download-proxy11`, squid, `Cache-Control: max-age=604800`) answered plain HEAD requests from a 6.4-day-old cache entry reporting the 1 November build, and a differently-headed HEAD got a 6 November build, while a GET returned the 8 November build. With `Cache-Control: no-cache` every response reported the current build (8 November, 362,586,112 bytes, ETag `"159ca000-65fbc0e0a1d71"`), matching the package downloaded this morning. The transport therefore always sends `no-cache`, and the older-data guard protects against any proxy that still serves stale content.

Not covered: a live update where Geofabrik has published a newer build than the active one (the guard and replacement paths are covered by tests), the app's choice of store location (`%LOCALAPPDATA%` is the expected root, decided at integration), and a progress/cancel UI.

## Stage 5: offline search and saved places

Completed 8 November 2026.

- **Tile decoder.** `mvt` reads Mapbox Vector Tiles (gzip, layers, attributes) and one representative point per feature; no third-party MVT dependency. Coordinates are checked against independently computed tile bounds.
- **Search index.** `search::build_index` reads every zoom-14 tile and writes a separate SQLite FTS5 file (`search.sqlite`) beside the package: places, named streets (one entry per street name per nearest town, placed on the street), points of interest, stations/stops/airports and named water. Streets and points of interest carry their nearest town as context. Common abbreviations match both ways (street/st, avenue/ave, north/n, saint/st, interstate/i and others), route numbers are searchable (`us 51`, `i 55`), the last word matches as a prefix, and leading or trailing compass words do not block a strong match (`main street` finds West Main Street). Ranking: strong matches (exact name, name prefix, route number, category such as `hospital` or `police`) before other word matches before matches through the nearest town; cities, towns and villages first; then by distance from the map position when one is given, otherwise by place size and population. The index records the package SHA-256 and is rejected for any other package.
- **Package preparation.** `update_region` builds the index after verification and installs package, record and index together; a package is not activated without search. `search::open_or_build` builds a missing or mismatched index for packages installed earlier.
- **Saved places.** `places::SavedPlaces` keeps user places in `saved-places.json` at the store root, outside region and version directories, so package updates and pruning never touch it. Names (1–120 characters, no control characters) and notes (up to 2,000) are validated; changes are written atomically and adopted only after a successful write; ids are never reused; an unreadable file is reported and left untouched, and `move_aside_unreadable` preserves it under a new name before a fresh list is started. `is_covered_by` reports whether the active package's bounds include a place, so the app can say a place is outside the map rather than requesting remote data.
- **Tests.** 45 unit tests and 2 integration tests pass; Clippy and rustfmt are clean.

Real Illinois package (8 November build):

- Index: 482,723 entries (6,905 places, 340,520 streets, 98,825 points of interest, 24,419 transit, 12,054 water), 97.8 MB, built in 15–17 s (decoding all 60,649 zoom-14 tiles takes about 8 s).
- Full update from the local source route: verification 6.1 s, indexing, activation; 23.5 s in total. The live store installed before search had its index built in place in 15.4 s on first search.
- Queries take 1–40 ms. Examples: `springfield` → Springfield (state capital); `carbondale` → Carbondale, then its fire stations; `main st clinton` → West and East Main Street near Clinton; `us 51` → streets signed US 51; near Carbondale, `hospital` → Memorial Hospital of Carbondale (0.4 km) and `police` → Carbondale Public Safety Center (0.6 km); near Clinton, `pharmacy` → CVS 1.5 km; near the Loop, `oak st` → East and West Oak Street.

Limits of this package, found while testing:

- **No fuel stations.** Geofabrik's Shortbread tiles contain no `amenity=fuel` in points of interest, buildings or sites. For emergency use this is a real gap; the raw `.osm.pbf` contains them (added in stage 6).
- **No address search.** House numbers carry no street names in this schema (added from the raw extract in stage 7).
- **No county search.** Boundary labels at every zoom are state names only, and land areas are unnamed (named parks still appear as points of interest).
- **State line.** The extract includes border towns in neighbouring states without state attribution, so `clinton` without a position ranks Clinton, Iowa (a city) above Clinton, Illinois (a town). With the map position supplied, Clinton, Illinois comes first when viewing central Illinois. The app must always pass the map position.
- Recently renamed roads match their current OSM name only (`lake shore dr` finds bike-share docks before DuSable Lake Shore Drive).

## Stage 6: fuel and charging stations from the raw extract

Completed 8 November 2026.

- **PBF reader.** `osm_pbf` reads OpenStreetMap PBF files with no new dependency. It handles blob framing with the specification's size limits, zlib and uncompressed blobs, and the header (bounding box, required features, data timestamp). Files needing anything beyond `OsmSchema-V0.6` and `DenseNodes` are refused. It reads nodes, dense nodes, ways and relations with tags. Blocks are decompressed and decoded on up to 8 worker threads.
- **Extraction.** `extras::extract` reads `amenity=fuel` and `amenity=charging_station` mapped as points, areas or multipolygons.
  - Areas are placed at the mean of their nodes. This needs a second pass for node positions and, for multipolygons, a third.
  - `access=private` or `no` is excluded.
  - Each station keeps name (else brand, else operator), brand, operator, opening hours, notable fuels and address. The fuels listed are diesel, truck diesel, biodiesel, E85, LPG, CNG and DEF; octane grades are left out.
  - An area and a point inside it count as one station when they are within 150 m and their names or brands agree. The more complete copy is kept and filled from the other.
  - Cities, towns, villages, hamlets and suburbs are read for nearest-town context.
- **Extras store.** Stations live in a per-region extras database (`<root>/<region>/extras/versions/<v>/extras.sqlite`, with `record.json` and an atomic `active.json`), independent of the map package.
  - It holds the stations, towns and search entries in the package index's format. When the search format changes, entries are rebuilt from the stored stations without the raw file.
  - `update_extras` follows the package update rules: probe, `no-cache`, ETag-checked resume through the same staging code, size and space checks, older-data guard and lock.
  - It hashes the download, extracts, and checks that the extract's bounding box covers the region and that it has towns. It then installs, activates and prunes, and deletes the raw file.
  - The record keeps the source URL, ETag, Last-Modified, size, SHA-256, OSM data time and counts.
  - A failed or cancelled extras update never touches the map package or the previous extras.
- **Search.** `SearchIndex::attach_extras` adds the stations to every search.
  - `gas`, `gas station`, `petrol` and `fuel` find fuel stations; `ev`, `charger` and `charging station` find charging stations. Brands and names work as before.
  - Results carry a detail line (brand, hours, fuels, address).
  - The search format moved to version 2 (detail column), so earlier package indexes are rebuilt on first open.
  - Search results with the same name are now kept apart unless they are within a few hundred metres. Previously, two same-brand stations in one town were collapsed into one.
- **Map.** The styles add a GeoJSON source (`/extras/stations.geojson`) with fuel and charging layers. Fuel uses the sprite fuel pump in amber; charging uses a green bolt drawn by the page. Fuel shows from zoom 11, charging from zoom 12, names from zoom 14. Both are placed above street names and route shields, so they win label collisions. Without extras the source is an empty collection.
- **Tests.** 52 unit tests and 2 integration tests pass; Clippy and rustfmt are clean. New tests cover:
  - the PBF reader: all element types, the header, unsupported features, truncation and corruption;
  - extraction: point, area, multipolygon, private exclusion, duplicate merging and detail text;
  - search with extras, entry rebuild after a format change, and GeoJSON;
  - the update flow: install, up to date, and replacement with pruning. An extract without towns and a damaged file are both rejected, and the active extras are kept.

Real Illinois extract: `illinois-latest.osm.pbf`, which redirects to `illinois-261107.osm.pbf`. It is 363,155,297 bytes, with Last-Modified 8 Nov 2026 03:22:32, ETag `"15a54f61-65fbb5b2457e2"` and SHA-256 `1da2b14a…4680d`; its OSM data time is 7 Nov 2026 21:21:36 UTC.

- **Counts:** 3,415 fuel stations and 464 charging stations, with 393 duplicate copies merged and none unlocated; 3,697 towns. Three reading passes take 2.5 s, and the extras file is 1.9 MB.
- **Update run:** `download-extras` through the local dev server was cancelled at 150 MB, then resumed, verified, extracted and activated in 6.5 s. The next run reported up to date with no GET. The raw file was removed from staging.
- **Search examples:**
  - Near Springfield, `gas` → Marathon 0.8 km (hours, address), then Amoco 1.0 km (open 24/7).
  - Near Clinton, `gas station` → Mach1, Phillips 66, Circle K, Casey's (0.5–1.0 km).
  - Near the Loop, `ev charger` → ChargePoint 0.3 km, EVIUM, Tesla Supercharger (open 24/7, address).
- **Map, in the desktop browser pane:**
  - Clinton at zoom 15 shows Casey's, Circle K and Mach1 with names. Phillips 66, 40 m from Mach1, gives way.
  - The Loop at zoom 15.5, dark, shows BP and ChargePoint.
  - 42 fuel stations are drawn around Springfield at zoom 11.5.
  - 0 map errors, 0 CSP violations, and no missing requests apart from the browser's favicon.

Limits:

- Coverage is whatever OSM mappers have entered; for example, there are only four fuel stations near Carbondale.
- About 10% of fuel stations are unnamed and unbranded, and are shown as "Fuel station".
- Each extras update needs a raw extract download of about 363 MB, in addition to the 363 MB package. The app should therefore offer extras updates separately from map updates.

## Stage 7: address search from the raw extract

Completed 8 November 2026.

- **Extraction.** The same reading passes now collect addresses: `addr:housenumber` with `addr:street` (or `addr:place` for rural addresses) on points, buildings and multipolygons.
  - A building is placed midway between its first node and the node halfway round its outline. For a simple outline this lies inside the building, and it needs two node positions per building instead of all of them.
  - Node positions for the second pass are held in a sorted table, not a hash map.
  - Interpolation lines (`addr:interpolation`, 209 in Illinois) and numbers longer than 16 characters are not used.
- **Towns.** An address keeps its `addr:city`. Otherwise it is assigned to the settlement whose covered radius best contains it: distance divided by a radius from `population` (about 20 km for Chicago, 2 km for a town of 25,000) or from the kind of place.
  - With plain nearest-town assignment, Chicago addresses without a city tag were split among neighbourhood and suburb names, and only 90,700 were found under Chicago; with the weighting, 480,186 are.
  - Suburbs and quarters within 2 km are kept as extra searchable words, so `1601 n clark lincoln park` works.
  - This is a heuristic, not municipal boundaries. Near a town line an address can be credited to the neighbouring town.
- **Storage.** The extras database (format 2) adds one row per street and town, with an FTS5 index over street, town and suburb names, and one compact row per house number. Positions are stored in millionths of a degree; the primary key is street and normalized number, without rowid.
  - Repeated numbers on one street are kept once: a point is preferred over a building outline, then the southernmost.
  - Streets are sorted, so the same extract always produces the same address data (checked by hashing two runs).
  - Address tables are carried over when search entries are rebuilt after a search-format change.
  - `update_extras` replaces active extras in an older format or that cannot be opened, even when the source is unchanged.
- **Search.** A query starting with a house number (digits with at most one letter, so `1st street` and `22nd` are ordinary searches) is matched against streets, towns and suburbs, then the number is looked up on each matching street.
  - Numbers are compared without spaces, punctuation or case (`101-A` = `101a`).
  - Exact matches come first: strong street matches before weaker ones, then by distance from the map position.
  - When no street has the number, each matching street offers its nearest listed number, labelled `2100 is not listed; nearest listed number on this street`.
  - Address results come before other results; a query that is not address-shaped is unaffected.
- **Tests.** 55 unit tests and 2 integration tests pass; Clippy and rustfmt are clean. New tests cover:
  - addresses on points, buildings and multipolygons, and town, suburb and duplicate handling;
  - exact and nearest-number search, house-number forms, and queries that are not addresses;
  - address tables surviving a rebuild, and an outdated extras file being replaced by an update.

Real Illinois extract (same file as stage 6):

- **Counts:** 843,142 addresses on 32,049 street-and-town groups. 35,070 were dropped as repeats or with nodes missing from the extract.
- **Size and speed:** the extras file is 34.9 MB, about 29 MB of it house numbers. Extraction takes 8.7 s with a peak working set of 189 MB.
- **Update run:** `download-extras` through the local dev server replaced the format 1 extras from the same source in 16.1 s (full transfer over loopback, verification, extraction, activation). The next run reported up to date.
- **Search examples** (2–5 ms each):
  - `905 e main st carbondale` → 905 East Main Street, Carbondale.
  - `906 east main carbondale` → 905, labelled as the nearest listed number.
  - `1601 n clark chicago` and `1601 n clark lincoln park` → 1601 North Clark Street, Chicago.
  - From Springfield, `100 n main` → 100 North Main Street in Chatham (14.7 km) and Athens (18.9 km).
  - `2100 s 6th st springfield` → 2101 South 6th Street, labelled as nearest.
- **Map:** unchanged. Clinton at zoom 15 still shows its fuel stations, with 0 errors and 0 CSP violations.

Limits:

- Coverage is OSM's. Chicago and a number of counties have imported addresses; many rural areas have few or none.
- Towns without a city tag are inferred from settlement points, not boundaries.
- Interpolated address ranges are not searched.
- The nearest listed number can be far from the one asked for on sparsely mapped streets. Its label says so, but it is not an interpolated position.

## Stage 8: WinUI app, ABI and installer

Completed 8 November 2026. Version 0.1.0.

- **C ABI.** `crates/offline-maps-ffi` builds `offline_maps_ffi.dll` (ABI version 1).
  - Exports: `om_abi_version`, `om_create`, `om_call` (a protobuf `Request` in, a `Response` out, from `proto/offline_maps.proto`), `om_tile`, `om_progress`, `om_cancel`, `om_result_bytes`, `om_result_free` and `om_destroy`. `om_tile` is the one bulk export and returns raw decompressed vector tiles.
  - Expected failures arrive as typed errors in the response; statuses are reserved for transport problems.
  - Panics are caught and leave the core unusable for normal calls; `om_cancel`, `om_progress` and freeing still work.
  - The core is safe to call from several threads. Tile reads, search and saved places each take their own lock, and an update holds none of them, so the map stays usable while a new one downloads.
  - Progress is polled, not called back. Cancellation never waits behind the running update.
  - Built with the static C runtime and Control Flow Guard, and with `panic = "unwind"` in release.
- **Managed bindings.** `OfflineMaps.Interop` uses `LibraryImport` and `SafeHandle` with disabled runtime marshalling. The native library loads only from the app folder. The ABI version is checked before anything else, request IDs are checked, and every result is copied before it is freed.
- **App.** WinUI 3 on .NET 10 and Windows App SDK 2.5.1, unpackaged and self-contained, single instance. It is a canvas workspace (style standard section 7):
  - Menu bar: File, Edit, View, Map, Places, Help.
  - Info bars for no map, download progress, errors and the stations-and-addresses offer.
  - The map, and a resizable search pane with saved places.
  - A status bar with the map and extras dates, and the map position.
  - A Settings window with the shared Theme and Accent color controls, distance units and the map folder.
  - Downloads ask first and state the size and source. Progress is shown in the info bar and the status bar, the map stays in use, and Cancel keeps the partial file for resuming.
  - Search runs off the UI thread, debounced, near the map centre, with stale results discarded.
  - Saved places can be added from the map centre, a search result or the map's context menu, then edited and removed (removal asks first).
  - Closing during a download asks first, cancels, waits for the core, saves the view and settings, closes the web view and releases the core.
  - Window placement and the map view are restored.
- **Map page.** `web/` holds the page MapLibre runs in. The shell sends theme colors, units, saved places, selections, zoom and reload commands as messages; the page reports the view, context menu, clicked places and shortcuts. Requests are served by the shell's request handler (see [Jupiter conformance](jupiter-exceptions.md)).
  - Finding: a WebView2 virtual host folder mapping bypasses `WebResourceRequested`. With one in place, tile and GeoJSON requests from MapLibre's worker failed with status 0, so the handler now serves the whole `map.local` origin.
- **Installer.** `pwsh build/build.ps1` is the release command. It runs `cargo fmt`, Clippy, the Rust tests and the interop tests, publishes, checks the published files and that the native binaries are x64, and builds `dist/OfflineMaps-0.1.0-win-x64-setup.exe` (279 MB, most of it the bundled WebView2 runtime).
  - Per-user install to `%LOCALAPPDATA%\Programs\Offline Maps`, with a top-level Start Menu entry and an uninstaller.
  - The offline WebView2 runtime installer is bundled, signature-checked at build time, and run only if the runtime is missing.
  - Third-party notices are generated by `tools/third-party-notices.py`.
- **Tests.** 55 core unit tests, 2 HTTP integration tests and 6 ABI tests (Rust), plus 9 managed/native boundary tests (C#). The boundary tests cover the ABI version, empty store, saved places with Unicode, typed creation errors, network errors, malformed and empty requests, unknown protobuf fields and concurrent calls. A real-store test runs with `OFFLINE_MAPS_TEST_STORE` (both sides pass against the Illinois store). The style check reports 0 violations.

Verified on Ian's machine (Windows 11, 200% scaling):

- **Development build** against the local Illinois store:
  - The map rendered from the core.
  - `905 e main st carbondale` → 905 East Main Street, shown with an accent marker beside Casey's (fuel layer).
  - A place was saved with Ctrl+D through the dialog: listed, selected and written to `saved-places.json`.
- **Installed app:**
  - Silent install; Start Menu shortcut and uninstall entry present.
  - Launched from the Start Menu shortcut: first-run state (no map, Download map).
  - With a scratch store and the dev server as source, Download map installed the 362.6 MB package and its search index through the app, with progress. The map appeared and the stations-and-addresses offer opened.
  - Download stations and addresses then installed the extras, and the map showed fuel stations.
  - Reinstalling over the installed copy (upgrade path, same AppId) and uninstalling left `%LOCALAPPDATA%\Jupiter\OfflineMaps` unchanged: settings and a partly downloaded Geofabrik package. Uninstalling removed the program, the shortcut and the uninstall entry. Reinstalling restored the shortcut, and the app launched from it.

Not covered:

- A launch with networking disabled.
- A full Geofabrik download through the installed app (one started from the installed app was stopped at 173 MB when the window was closed; it resumes from there).
- A clean machine without development tools.
- Screen reader, high contrast and text scaling review.
- The light theme of the app shell.
- Code signing.

Known limits:

- Closing takes a few seconds while WebView2 shuts down.
- Saved places are shown on the map but are not separately selectable when a search marker covers them.
- The app's own license is not yet chosen (About says so).

## 0.1.1: search results and ranking

Completed 8 November 2026. Reported by Ian on 0.1.0: search did not seem connected to the map, and suggestions only appeared once the whole name was typed.

- **Clicking a result did nothing.** The empty-list message shares the results list's grid cell and was drawn over it, taking every mouse click. Enter worked because it selects the first row in code, so search appeared to need the full name. The message no longer takes clicks.
- **Partial names ranked nearby villages first.** Near the map centre, settlements were ordered by distance alone, so `carb` near Evanston listed Carbon Hill and Carbon Cliff before Carbondale. Settlements now come by exact name, then size (city, town, village), then distance. Streets, points of interest and stations are still ordered by distance. New test: `spring` near Chicago puts Springfield before a nearer village; the village's full name puts it first.
- **Zoom on selection.** A selected result is shown at its own zoom (a city at 11, a village at 13, an address at 18). Before, the map never zoomed out, so a town picked while at street level stayed at street level.

Verified in the installed app (upgraded silently from 0.1.0; Start Menu entry, uninstall entry at 0.1.1, map store and settings kept): launched from the Start Menu, `carb` listed Carbondale first, and a mouse click on Carbon Hill selected it and moved the map there at zoom 13.

## 0.2.0: more regions

Completed 8 November 2026. Ian asked for other mapsets to be downloadable, to fill the map out to other states.

- **Catalog.** 52 regions from Geofabrik's index (see [sources](sources-and-architecture.md#regions-020)), with approximate download sizes. Alaska is left out (29.6 GB package spanning the antimeridian).
- **Core.** The service holds every installed region: tiles are read from each package that touches them and merged on borders, search ranks all regions' results together, stations come from all extras, and saved places count as covered inside any installed map. New commands: `GetCatalog`, `RemoveRegion`; `Update` names a region. The status lists installed regions and the box around them, and a content version that changes with any map or extras.
- **App.** Map > Regions… opens a Regions window (collection workspace): filter, list of regions with their state, and a pane with Download or Update map, Download or Update stations and addresses, and Remove (asks first; Cancel is the default). Downloads are queued and run one at a time in the map window's progress bar; Cancel stops the current one and clears the queue. Map > Update all regions… queues every installed map and extras. The no-map and stations messages and Map information cover several regions.
- **Tests.** Core: catalog consistency, tile merging (joined layers, renumbered tags, duplicates, bad tags), search across two indexes. ABI: catalog, removal of an absent and an unknown region, updates naming an unknown region. Interop: catalog and removal. 59 core, 2 HTTP and 6 ABI tests (Rust) and 9 interop tests (C#) pass; the real-store test passes against the Illinois store.

Verified in the installed app (upgraded silently from 0.1.1; Illinois map, extras and settings kept; launched from the Start Menu):

- Regions listed 52 regions; filtering "indi" left Indiana, "About 248 MB".
- Download map for Indiana showed progress in its row ("Map: Downloading: 58 MB of 248 MB"), then installed and prepared search in about 25 seconds. The status bar read "Completed: the Indiana map installed" and the stations offer named Indiana.
- Searching "hammond" listed Hammond, Indiana (city) before Hammond, Illinois (village). At zoom 10 and 14 the map was continuous across the state line at Calumet City and Hammond, with labels on both sides and no doubled labels.
- Remove Indiana asked first, deleted `us-in`, and the map and search went back to Illinois alone. A message bug found here ("Completed 0 downloads and updates" after a removal) was fixed before release.

Not covered: stations and addresses for a second region (a further 202 MB for Indiana), two downloads queued at once, and cancelling a queued download in the installed app.

## 0.3.0: terrain

Completed 8 November 2026. Ian asked whether other mapsets could add layers such as topography. Terrain comes from Mapterhorn (see [sources](sources-and-architecture.md#terrain-030)).

- **Measured first.** A script read only the archive's index to size each region at zoom 0–12 (Illinois: z12 3,901 tiles, 271 MB; z0–12 366 MB; 0.2 MB of index read). Zoom 12 was chosen: about 15 m per pixel, enough for shading and street-level contours; zoom 13 would quadruple the size. All 52 regions are in the catalog with their terrain size (`tools/make-regions.py`, 4th input).
- **Core.** `pmtiles.rs` reads a PMTiles v3 index through range requests (header, root and leaf directories, Hilbert tile IDs). `terrain.rs` downloads a region's tiles in joined ranges with `If-Range`, resumes after cancelling, detects a file replaced on the server mid-download, checks the tile count, and installs atomically. `Transport` gained `fetch_range` (an exact `Range` request; HTTP 200 to an `If-Range` request means the file changed).
- **ABI and app.** Update target `TERRAIN` (needs the region's map), `GetTerrainTile`, terrain state per region and the box around all terrain in the status. The Regions window has Download or Update terrain with its size; View > Terrain shading and View > Contour lines (kept in settings, on by default); Map information and the third-party notices carry the Copernicus notice.
- **Tests.** PMTiles: tile IDs against the specification's reference value, root and leaf directory lookup. Terrain (fake archive server): download and serving, up to date on a second run (3 requests: header, root, one joined range), replacement by a newer file and pruning of the old one, a file changed mid-download, cancel and resume, and Illinois's zoom-12 tile count matching Mapterhorn's index (3,901). ABI and interop: empty terrain tile, terrain refused without a map. 66 core, 2 HTTP and 6 ABI tests (Rust) and 9 interop tests (C#) pass.

Verified in the installed app (upgraded silently from 0.2.0; launched from the Start Menu):

- Regions showed Illinois terrain as about 366 MB; Download terrain showed progress in the row and installed in about 70 seconds: 5,309 tiles, 365.6 MB, with the archive's ETag recorded.
- A bug found here: the terrain layers were never added, because `map.isStyleLoaded()` is false inside MapLibre's `style.load` while tiles are still loading. The page now tracks style readiness itself. Fixed before release.
- At Galena (zoom 12 and 14) hillshading and contour lines followed the river valleys under the roads and labels, contour labels read "600 ft", "700 ft" and "750 ft", and street and town labels took precedence. The map credited "Terrain: Mapterhorn, USGS, Copernicus".
- View > Terrain shading turned the shading off and on, leaving the contours, and was kept in settings. The terrain credit now stays while only contours are shown (fixed after this check).

Not covered: terrain for a region other than Illinois, metric contours, the light theme with terrain, and an update of terrain when Mapterhorn publishes a new archive.

## 0.3.1: Show whole map

Completed 8 November 2026. Reported by Ian: View > Show whole map stopped short of the installed maps (with Illinois and Indiana: zoom 6.0, southern Illinois and the top of both states cut off). The view was only allowed half a degree beyond the maps, and MapLibre does not zoom out past that limit, so a tall box in a wide window could not fit. The limit now leaves the box's larger side free in every direction, and a first launch opens on the whole map. Verified in the installed app: zoom 5.7 with both states in view.

## 0.4.0: outdoor places

Completed 8 November 2026. The first of three layers Ian approved (outdoor places, flood zones, public lands).

- **Extraction.** The extras now also keep shelters (`amenity=shelter` except public-transport shelters, wilderness and alpine huts), drinking water (`amenity=drinking_water` and `water_point`, taps and springs marked drinkable; not water marked undrinkable), campgrounds (`tourism=camp_site` and `caravan_site`) and public toilets. Private places, and customers-only shelters and toilets, are left out. Extras record an extraction version; older extras are rebuilt on update even from an unchanged data file.
- **Map and search.** Four layers draw them from the stations source with icons drawn by the page (shelters from zoom 12, campgrounds 11, water 13, toilets 14; names from 15); View > Outdoor places turns them off. Search finds them by name or by words such as "campground", "water", "shelter", "restroom".
- **App.** Regions and Map information show the counts; regions whose extras predate outdoor places get an offer to update them.
- **Tests.** Classification rules (including bus shelters, undrinkable water and customers-only toilets) and extraction with names; 68 core tests pass.

Verified in the installed app (upgraded from 0.3.1): the offer named Illinois and Indiana; the update rebuilt both in about 70 seconds (Illinois 3,363 shelters, 951 drinking water, 516 campgrounds, 1,861 toilets; Indiana 1,635, 288, 407, 848). Searching "campground" and "shelter" listed nearby places with their towns; around Lincoln Park Zoo the map showed toilets, drinking water and shelters with labels at zoom 15. Found here and fixed before release: the offer's button said "Download" for regions that only needed an update. Park names are not searchable: Shortbread tiles carry no names for land areas.

## 0.5.0: flood zones and public lands

Completed 8 November 2026. Ian chose prebuilt overlay files published on GitHub over converting government files on each computer.

- **Sources checked.** FEMA's Map Service Center answered a search and served the Illinois state file to a plainly named client (no browser imitation). ScienceBase, the bulk PAD-US host, redirected file downloads to a sign-in page (its download page forces a Keycloak login, and its signed-link API refuses anonymous requests), so PAD-US comes from the public USGS feature service instead.
- **Build tool.** `tools/layers/build_overlays.py` and `overlays.yml`; see [sources](sources-and-architecture.md#flood-zones-and-public-lands-050). Illinois: 70 MB, 43,361 tiles.
- **Core and app.** `overlays.rs` downloads (resumably, through the shared staging download), checks the layers and region, records the sources stamped in the file and installs; the service serves and merges overlay tiles; the Regions window has Download or Update flood zones and public lands; View > Flood zones and View > Public lands; Map information has a colour key and the sources. `OFFLINE_MAPS_OVERLAYS_URL` points the app at another server for testing.
- **Tests.** Overlay install, up-to-date check, replacement and pruning, and refusal of a file without the overlay layers; empty overlay tile through the ABI. 69 core, 2 HTTP and 6 ABI tests (Rust) and 9 interop tests (C#) pass.

Verified in the installed app, downloading from a local server holding the Illinois file: Regions showed the download's progress and then "Flood zones and public lands: 70 MB; flood data effective 07/21/2026, USGS PAD-US 4.1". At Havana the Illinois River floodplain was shaded as flood zones, Emiquon National Wildlife Refuge and the Illinois River Floodplain Complex showed as public land with labels, and the map credited FEMA and USGS. Fixed after this check (the build data confirms the labels; the map was not looked at again because Ian was using the computer): labels without PAD-US's "Fee" suffix and none for easements, lighter flood shading, a shorter credit, wrapping command names in the Regions pane, and a terrain decoding message that filled the log where no region's terrain covers a tile.

Not yet done: the `overlays` release on GitHub, which the app's default address points to (`https://github.com/ianrastall/offline-us-maps/releases/download/overlays/<region>-overlays.mbtiles`). Until it exists, the download fails with a network error.

## 0.5.1: overlay files for every region

Completed 9 November 2026. Ian asked for all regions to be built.

- **Build.** `build_overlays.py --all` built all 52 regions in about 4.8 hours of build time, 4.78 GB together: from 0.8 MB (US Virgin Islands) to 322 MB (Iowa, 839,273 flood areas); Florida and Texas 259 MB each. 4,975,627 flood areas and 459,346 public land areas in all. FEMA downloads and intermediate files were deleted after each region; the files and `summary.json` are in `data/layer-build/out` (not in git).
- **No FEMA state file.** FEMA's search listed none for Alabama, Idaho, Louisiana, Washington and the US Virgin Islands; these were read from FEMA's NFHL map service (see [sources](sources-and-architecture.md#flood-zones-and-public-lands-050)). Fetching each cell's shapes failed with HTTP 500, as did single very large areas, so the build lists IDs per cell and reads areas by ID with coordinates to six decimals, simplifying an area to 0.5 m only if it still fails (one in Alabama).
- **Older flood data.** Most state files are effective 2024–2026; Connecticut is 2023, Delaware 2020, Vermont 2015, DC 2010 and Puerto Rico 2009, as FEMA publishes them.
- **Catalog.** Regions carry the overlay file's size (`overlay_bytes`, from `summary.json` through `make-regions.py`); the Regions window shows it, and says "not available for this region yet" with the button disabled when it is 0.
- **Build environment.** The user `DOTNET_ROOT` points to `D:\lang\dotnet\sdk`, which lacks the pinned SDK 10.0.114; the installer was built with `DOTNET_ROOT` cleared for that process so the SDK in `C:\Program Files\dotnet` was used.

Verified: 69 core, 2 HTTP and 6 ABI tests (Rust) and 9 interop tests (C#) pass; the 0.5.1 installer installed over 0.5.0, the uninstaller entry reads 0.5.1, and the app started from the Start Menu entry. Read through UI Automation, the Regions window showed "About 51 MB" for Indiana, 166 MB for Louisiana and 1 MB for the US Virgin Islands, with the download button enabled.

## 0.6.0: Offline US Maps, public domain

Completed 9 November 2026, for publication on GitHub as Ian decided on 8 November ([project brief](project-brief.md#publication-decided-8-november-2026)).

- **Name.** Offline US Maps in windows, dialogs, the installer, Start and Settings > Apps. The installer keeps the AppId, so it upgrades the earlier install in place (same folder, `%LOCALAPPDATA%\Programs\Offline Maps`), and removes the old "Offline Maps" Start and desktop shortcuts. The data folder (`%LOCALAPPDATA%\Jupiter\OfflineMaps`), the AppUserModelID and the mutex are unchanged. The setup file is now `OfflineUSMaps-<version>-win-x64-setup.exe`.
- **Licence.** The Unlicense in `LICENSE`, installed as `LICENSE.txt`; About and the third-party notices say so, and the notices now name FEMA and USGS as the overlay sources. Cargo crates declare `Unlicense`. Jupiter.UI is released under the same licence (commit ea649e7 there).
- **Readme.** Rewritten for people installing from GitHub: install, features, data terms, and cloning Jupiter.UI beside the repository to build.

Verified: build script (fmt, clippy, 69 core, 2 HTTP and 6 ABI tests, 9 interop tests); 0.6.0 installed over 0.5.1; Settings > Apps lists Offline US Maps 0.6.0 under the same AppId; Start has "Offline US Maps" and no longer "Offline Maps"; installed maps (Illinois, Indiana) were kept; the app started from the new Start entry with the window titled Offline US Maps.

Published 9 November 2026: repositories [ianrastall/offline-us-maps](https://github.com/ianrastall/offline-us-maps) and [ianrastall/jupiter-ui](https://github.com/ianrastall/jupiter-ui) (public). Jupiter.UI's `Detach` methods on the theme and accent services, which this app has used since 0.1.0 but which were never committed there, were committed (60a62d4) so the published library builds the app. A clean clone of both repositories, side by side, built the app with no warnings and passed the interop tests. Releases: [v0.6.0](https://github.com/ianrastall/offline-us-maps/releases/tag/v0.6.0) with the installer (not code-signed) and `SHA256SUMS.txt`, and [overlays](https://github.com/ianrastall/offline-us-maps/releases/tag/overlays) with all 52 overlay files, whose GitHub SHA-256 digests match the build manifests. The installed release build downloaded Indiana's flood zones and public lands from GitHub in Map > Regions (51 MB, flood data effective 07/07/2026).

## 0.7.0: whole-country overview and update checks

Completed 9 November 2026. Ian asked for a map of the whole country, filled in by the downloaded regions, and said Update gave no sign of whether new maps were needed and downloaded everything again.

- **Why Update downloaded again.** The check was already there: an update compares the server's ETag, size and Last-Modified with the installed file and downloads nothing when they match. But Geofabrik rebuilds both the Shortbread package and the raw extract daily (on 9 November the Illinois package was dated 04:10 GMT that day), so nearly every update found a newer file, and the app gave no way to see that before downloading.
- **Check without downloading.** `Update` takes `check_only`: each update function returns `Available` (with the newer file's date and size) where it would have downloaded. Map > Check for updates replaces Update all regions: it checks every installed part, then either says everything is up to date or lists the newer parts with their dates, the installed dates and sizes, and offers to download them. In Map > Regions, Check for updates does the same for one region; each part's line then says whether it is the newest version, and an installed part has a download button only when a newer version was found.
- **Overview.** `tools/overview/build_overview.py` builds `assets/overview/us-overview.mbtiles` (3.7 MB, zoom 0–7, committed) from Natural Earth 1:10m, public domain: ocean and lakes, rivers, borders between states and with Canada and Mexico, Interstates and US and state highways, cities and capitals, state names. Features in a state or territory carry its region ID (borders carry both neighbours'). The core serves the file at `overview/{z}/{x}/{y}.pbf`. The page draws it under everything; below zoom 6 it is the whole map (the base packages are not drawn there: their low-zoom tiles carry names for the whole country, which doubled the overview's); from zoom 6 its features inside installed regions are left out. State and country names come only from the overview. With no region installed, the map opens on the 48 contiguous states, and the view can move over the whole country.

Verified: Rust tests (70 core including a check-only test, 2 HTTP, 6 ABI including an empty overview tile) and the app build with no warnings. In the development build with Illinois and Indiana installed: the whole country at zoom 3.7 with one set of names; Michigan's border with Indiana not drawn twice; at zoom 11 on the Chicago lakefront, no overview water over land; with an empty profile, the overview of the 48 states with the No map is installed bar. Check for updates found the Illinois map (downloaded that morning) and both regions' terrain current, and listed both regions' stations and addresses (Geofabrik's file of 9 November) and Illinois's flood zones (installed from a local test copy, so the GitHub file counts as different), 635 MB together; Not now downloaded nothing.

Released 9 November 2026 as [v0.7.0](https://github.com/ianrastall/offline-us-maps/releases/tag/v0.7.0), built from 7d11e5b; the installer's SHA-256 (25e77b62…dbb6b) matches GitHub's digest. 0.7.0 installed over 0.6.0 here: Settings > Apps lists Offline US Maps 0.7.0 under the same AppId, `Overview\us-overview.mbtiles` is in the program folder, and the app started from the Start entry with the overview around Illinois and Indiana.

Known, not changed: some place labels near the Chicago lakefront at zoom 11 lose letters ("Maple Park" drawn as "Ma le Par k"). The installed 0.6.0 shows the same at that view, so it predates this release.

## Next stages

1. Download the Illinois map and the stations and addresses from Geofabrik in the installed app, then launch with networking disabled and verify map rendering, labels, search, stations, addresses and saved places from a clean process. This is the decisive first-workflow acceptance test.
2. Accessibility and appearance review: keyboard order, screen reader names, high contrast, text scaling, the light theme and mixed-DPI monitors.
3. Install on a clean Windows 11 machine.
4. If town attribution near municipal lines proves wrong in use, assign towns from administrative boundaries (`admin_level=8`) in the extract instead of settlement points.
