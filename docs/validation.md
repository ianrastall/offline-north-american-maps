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

## Next stages

1. Implement real download transport with finite timeouts, cancellation, size checks, ETag-checked resume and SHA-256 verification into staged storage outside the install directory; verify with `verify_package`, write the `PackageRecord` and activate atomically, keeping the previous package through any failure.
2. Build offline place search and saved places. Verify Illinois boundary behavior and show package coverage instead of silently requesting remote data.
3. Integrate Rust through the canonical ABI into the Jupiter WinUI workspace. Validate keyboard, resizing, accessibility, light/dark and accent settings.
4. Produce the installer through the normal release command. Install and launch from Start Menu. Test an upgrade and uninstall while preserving user data.
5. Launch with networking disabled and verify map rendering, labels, search and saved places from a clean process. This is the decisive first-workflow acceptance test.
