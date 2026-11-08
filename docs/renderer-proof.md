# Renderer proof: Illinois Shortbread tiles in MapLibre

8 November 2026 · Stage 1 of [implementation stages](validation.md).

## Question

Can Geofabrik's Illinois vector tile package be rendered as a labeled road map entirely from local files, with no remote style, glyph, sprite or tile requests?

Result: **yes**. MapLibre GL JS rendered the package from a loopback server under a same-origin Content Security Policy. All requests stayed on the loopback origin and no CSP violations were reported. Labels, icons and roads render from zoom 7 to zoom 17.

## Inputs

| Item | Version / source | SHA-256 | License |
|---|---|---|---|
| `illinois-shortbread-1.0.mbtiles` | Geofabrik, Last-Modified 8 Nov 2026 04:12 UTC, ETag `"159ca000-65fbc0e0a1d71"`, 362,586,112 bytes | `17c12c9500b5b1c7175a88d605c30dc4f25430b72be69e77403d7261ba227cb9` | ODbL 1.0 (OSM contributors, Geofabrik) |
| MapLibre GL JS | npm `maplibre-gl` 6.16.0 (25 Oct 2026), lockfile integrity in `proof/renderer/package-lock.json` | tarball `1504a180…5de4b` | BSD-3-Clause |
| VersaTiles Style `styles.tar.gz` | v7.0.1 (15 Oct 2026) | `088bbfb9…a6e41` | MIT |
| VersaTiles Style `sprites.tar.gz` | v7.0.1 | `3dfed482…f10b65` | CC0 1.0 |
| VersaTiles Fonts `noto_sans.tar.gz` | v3.0.0 (14 Sep 2026) | `f58be6be…0e334e` | Noto Sans: OFL 1.1; packaging: MIT |

Full hashes are pinned in `proof/renderer/fetch-assets.mjs`, which refuses mismatches. Versions newer than about two weeks were deliberately not used. Map data and extracted assets live under `data/` and are not committed.

## Package contents

- MBTiles (SQLite) with `PRAGMA quick_check` = ok. Metadata: name Shortbread, version 1.0, format pbf, zoom 0–14, bounds −91.51608, 36.96731, −87.49264, 42.50941, author "OpenStreetMap contributors, Geofabrik GmbH", license "Open Database License 1.0".
- 81,302 gzip-compressed Mapbox Vector Tiles: 60,649 at zoom 14 (228 MB), 15,344 at zoom 13 (44 MB), the remainder under 50 MB.
- Shortbread layers include `streets`, `street_labels` (with `name`, `name_en`, `ref`), `place_labels`, `pois`, `water_*`, `land`, `buildings`, `boundaries`, `public_transport`.
- Geofabrik republished the file between 1 and 8 November (Sunday builds observed both weeks). Geofabrik still labels the package experimental.

## Method

The proof server (originally `proof/renderer/serve.mjs` using Node 24's built-in `node:sqlite`; replaced on 8 November by the Rust `offline-maps-dev serve`, see [stage 3](validation.md)) binds 127.0.0.1 only and serves:

- `/tiles/{z}/{x}/{y}.pbf` from the MBTiles file, converting XYZ to the TMS row order MBTiles uses and passing gzip bodies through with `Content-Encoding: gzip`. Missing tiles return 204.
- MapLibre's `dist/`, the VersaTiles styles, sprites and glyph ranges.
- Every response carries `default-src 'self'; connect-src 'self'; worker-src 'self' blob:; …; report-uri /csp-report`. Violations are logged and exposed at `/proof/stats`.

`proof/renderer/public/main.js` loads a published VersaTiles style unchanged except for rewriting its tile, glyph and sprite URLs to the loopback origin, selecting Mercator projection, restricting panning to the package bounds and replacing the attribution with OSM/Geofabrik attribution.

Rerun:

```powershell
cd proof/renderer; npm ci; node fetch-assets.mjs; cd ../..
node tools/build-styles.mjs
cargo run --release -p offline-maps-dev -- serve data/sources/illinois-shortbread-1.0.mbtiles
```

Then open http://127.0.0.1:8765/ (append `?theme=dark` for the dark style). The page now uses the derived Offline Maps styles rather than the stock VersaTiles style tested here.

## Results

Checked in the Claude desktop browser pane (Chromium) on Windows 11.

| View | Result |
|---|---|
| Statewide, zoom 7 | Towns, highways, rivers, state label. First idle 861 ms from a cold page load. |
| Chicago Loop, zoom 15 (overzoomed past 14) | Street names (Michigan, Wabash, LaSalle, Madison, Wacker…), CTA stations, districts, parks, landmarks. |
| Carbondale, zoom 17 | Clean overzoom; street names, one-way arrows, POI icons, building footprints. |
| Clinton/De Witt County, zoom 10 | Towns, roads, lakes legible. Route numbers **not** shown by the stock style (see findings). |
| Springfield, zoom 13, `colorful-dark` | Dark theme renders correctly; first idle 533 ms. |
| Network | Browser network log: every request went to the loopback origin. Server: 109 requests, 54 tiles (4.5 MB), 0 empty tiles, 0 CSP violations; only 404 was the browser's own `/favicon.ico`. |
| Errors | 0 MapLibre errors in every view. |

A first-idle reading of 19.9 s was recorded while the browser pane was hidden and the page was being throttled as a background tab; it is not a renderer measurement.

## Findings

1. **Route numbers need a tuned style.** The tiles carry `ref` values: at zoom 10 for motorways (I 55, I 74, I 155, I 72, "I 74 / US 51") and at zoom 12 for trunk/primary roads (US 51, IL 10, IL 54). VersaTiles shows motorway refs only from zoom 14 and never shows US/state route refs. Lowering the motorway shield layer to zoom 8 at runtime made interstate numbers appear at zoom 10. For emergency driving, Offline Maps needs its own style derived from VersaTiles (MIT) with interstate, US and state route labels at regional zooms and shield-like treatment.
2. **Glyph storage.** Noto Sans regular + bold glyph ranges are 77 MB for the full Unicode range (154 MB for all four Noto Sans faces). The style uses only regular and bold. Subsetting to the ranges present in the package's names is an installer-size optimization, not a correctness issue. Styles are 15.5 MB for 30+ variants; the app needs two (light and dark). The base sprite sheet is 0.36 MB.
3. **MapLibre 6 is ESM-only** and resolves its module worker relative to its own URL. Serving `dist/` from the same origin as the page works under a strict same-origin CSP with no blob or remote script allowance beyond `worker-src blob:`.
4. **Data date.** The package metadata contains no OSM data timestamp. The download's `Last-Modified`/`ETag` must be recorded as the source date proxy; the raw `.osm.pbf` page reports the replication timestamp separately. The app must not imply the tile package is current beyond that.
5. **Not yet proven:** WebView2 specifically (the proof used Chromium in the desktop app; WebView2 is the same engine), serving tiles from the Rust core rather than Node, behavior with the network adapter actually disabled (stage 6), and memory use over long sessions.

## Decision

Adopt **MapLibre GL JS rendering local Shortbread vector tiles inside WebView2** for the map surface, served through WebView2's virtual host and request interception rather than a network listener:

- Static assets (MapLibre, style, sprites, glyphs) bundled with the app and mapped through `SetVirtualHostNameToFolderMapping`.
- Tiles served on request through `WebResourceRequested` from the Rust core, which owns the MBTiles package (read-only SQLite) and its validation. No TCP listener in the shipped app.
- A same-origin CSP equivalent to the proof's.
- The WinUI shell keeps the menu bar, regions/search pane, status bar, Settings and Jupiter.UI appearance.

This is the same content-rendering exception pattern as Lichess Local, recorded in [jupiter-exceptions.md](jupiter-exceptions.md). The WinUI 3 MapControl was not pursued because it is backed by the online Azure Maps service. A native .NET renderer (for example Mapsui) was not evaluated in this proof; MapLibre was chosen because it renders this package and published Shortbread styles directly, with proven offline glyph and sprite handling.
