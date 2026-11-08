# Implementation and validation

## Stage 1: project and core foundations

Implemented: Illinois source references, attribution constants, finite/range-checked coordinates, package readiness states and bounded streaming to caller-owned staging storage. Unit tests cover multibuffer transfers, unknown lengths, truncation, empty bodies, size limits, cancellation, write failures and readiness.

This is not an HTTP download or an offline map demonstration. The caller must supply a response reader and staging writer and validate package format separately.

Verified on Windows x64 on 27 October 2026 with Rust 1.99.1: all 8 unit tests passed; Clippy passed with warnings denied. Formatting checked with rustfmt. No dataset download or installed-app verification has been performed.

## Stage 2: renderer proof

Completed 8 November 2026; see [renderer proof](renderer-proof.md). Geofabrik's Illinois Shortbread MBTiles (362.6 MB, ODbL, zoom 0–14, 81,301 tiles) rendered in MapLibre GL JS 6.16.0 with a bundled VersaTiles 7.0.1 style, sprites and Noto Sans glyphs. All requests stayed on the loopback origin under a same-origin CSP with zero violations and zero map errors, from statewide zoom 7 to overzoomed zoom 17, in light and dark styles. No preparation step is needed: the package is directly renderable. Decision: MapLibre in WebView2 for the map surface, recorded as a Jupiter exception. Finding: the stock style hides interstate/US/state route numbers at regional zooms although the tiles contain them; Offline Maps needs its own derived style.

Not covered: WebView2 itself, Rust-served tiles, a network-adapter-off launch and long-session memory.

## Next stages

1. Derive the Offline Maps light/dark style: route labels at regional zooms, OSM/Geofabrik attribution, glyph subsetting. Serve tiles from the Rust core (read-only MBTiles) and record package metadata (ETag, Last-Modified, SHA-256, byte length).
2. Implement real download transport with finite timeouts, cancellation, size checks, checksum verification and version-safe resume; preserve previous packages through failures.
3. Build offline place search and saved places. Verify Illinois boundary behavior and show package coverage instead of silently requesting remote data.
4. Integrate Rust through the canonical ABI into the Jupiter WinUI workspace. Validate keyboard, resizing, accessibility, light/dark and accent settings.
5. Produce the installer through the normal release command. Install and launch from Start Menu. Test an upgrade and uninstall while preserving user data.
6. Launch with networking disabled and verify map rendering, labels, search and saved places from a clean process. This is the decisive first-workflow acceptance test.
