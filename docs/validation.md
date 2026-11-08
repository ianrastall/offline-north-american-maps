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

## Next stages

1. Integrate Rust through the canonical ABI into the Jupiter WinUI workspace. Validate keyboard, resizing, accessibility, light/dark and accent settings.
2. Produce the installer through the normal release command. Install and launch from Start Menu. Test an upgrade and uninstall while preserving user data.
3. Launch with networking disabled and verify map rendering, labels, search and saved places from a clean process. This is the decisive first-workflow acceptance test, including fuel and charging stations and addresses.
4. If town attribution near municipal lines proves wrong in use, assign towns from administrative boundaries (`admin_level=8`) in the extract instead of settlement points.
