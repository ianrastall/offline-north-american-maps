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
- **Live Geofabrik check (HEAD only, nothing downloaded).** TLS through SChannel works; 404 and an expired certificate fail with clear errors.

**Geofabrik caching finding.** Geofabrik's download proxy (`download-proxy11`, squid, `Cache-Control: max-age=604800`) answered plain HEAD requests from a 6.4-day-old cache entry reporting the 1 November build, and a differently-headed HEAD got a 6 November build, while a GET returned the 8 November build. With `Cache-Control: no-cache` every response reported the current build (8 November, 362,586,112 bytes, ETag `"159ca000-65fbc0e0a1d71"`), matching the package downloaded this morning. The transport therefore always sends `no-cache`, and the older-data guard protects against any proxy that still serves stale content.

Not covered: a full live download through the new transport (the earlier 8 November download used curl), the app's choice of store location (`%LOCALAPPDATA%` is the expected root, decided at integration), and a progress/cancel UI.

## Next stages

1. Build offline place search and saved places. Verify Illinois boundary behavior and show package coverage instead of silently requesting remote data.
2. Integrate Rust through the canonical ABI into the Jupiter WinUI workspace. Validate keyboard, resizing, accessibility, light/dark and accent settings.
3. Produce the installer through the normal release command. Install and launch from Start Menu. Test an upgrade and uninstall while preserving user data.
4. Launch with networking disabled and verify map rendering, labels, search and saved places from a clean process. This is the decisive first-workflow acceptance test.
