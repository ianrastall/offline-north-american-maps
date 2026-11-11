# North America and Android plan

10 November 2026. Direction agreed by Ian on 10 November 2026 ([project brief](project-brief.md#north-america-and-android-direction-agreed-10-november-2026)). Stage 0 passed on 11 November 2026; stage 1 (Mexico) was released in 0.8.0 the same day.

## Purpose

The app is a survival tool for a serious national or world crisis, when electricity is available but internet is not. Two changes follow:

- Coverage extends to Canada and Mexico, and Alaska is added. People in a crisis cross borders, and Alaska is reachable by land only through Canada.
- An Android app joins the Windows app. The two have different jobs:

| | Windows (the "guy in the chair") | Android phone |
| --- | --- | --- |
| Main job | Prepare: download over a good connection, process, review | Carry: use the map away from the computer |
| Position | None | GPS, which works without internet |
| Heavy processing | Yes: reading the raw OSM extracts for stations, addresses and outdoor places | Avoided where possible; receives prepared files |
| Overlays and terrain | All | As storage allows |
| Getting maps | Downloads from the servers | Copied from the desktop over USB (no internet needed), or downloaded directly when online |

## Facts checked 10 November 2026

**Geofabrik Shortbread packages** (HEAD requests to `download.geofabrik.de`, `Cache-Control: no-cache`):

| Region | Shortbread package | Raw extract (`.osm.pbf`) |
| --- | --- | --- |
| Mexico (one region, not split by state) | 1.92 GB | 648 MB |
| Canada (country file) | none | 6.51 GB |
| Each Canadian province and territory | none (all 13) | 12 MB (Prince Edward Island) to 1.51 GB (Nunavut); Ontario 1.03 GB, Quebec 1.17 GB, British Columbia 1.25 GB |
| Alaska | 29.6 GB (spans the world: the outline crosses the 180th meridian) | 144 MB |

So Mexico can use Geofabrik's package as the US regions do. Canada and Alaska need tiles built from the raw extracts by the maintainer and published, as the flood zones and public lands already are.

**Storage on this machine.** Installed with every optional download: Illinois 895 MB, Indiana 582 MB. Base maps for the lower 48 and DC: about 14 GB. All 52 overlay files: 4.8 GB. All 52 regions' terrain: about 23 GB. A phone with 128–256 GB holds a home region and its neighbours easily. Memory is not a constraint: the map engine reads only the tiles on screen.

**Glyphs.** The bundled Noto Sans ranges include Latin, Greek and Cyrillic, which covers French and Spanish names. Inuktitut syllabics (U+1400–167F, used in Nunavut and northern Quebec) are not bundled.

**Overlays.** FEMA flood zones and USGS PAD-US cover the US only. Terrain from Mapterhorn is worldwide (Copernicus GLO-30 outside the US), so it works for Canada and Mexico unchanged.

## Stages

### 0. Offline acceptance test of the existing app

Stage 1 of [validation's next stages](validation.md#next-stages): download Illinois and its stations and addresses in the installed app, then launch with networking disabled and verify the map, labels, search, stations, addresses and saved places. It is the app's central promise, so it comes before any expansion. **Passed 11 November 2026** ([validation](validation.md#offline-acceptance-test)).

### 1. Mexico

- Add Mexico to the region catalog (`tools/make-regions.py`) and run the existing coverage and layer checks on its package.
- Search: accent-insensitive matching and Spanish street words (calle, avenida, carretera, colonia) alongside the US abbreviations; check how Mexican addresses are tagged in the extract.
- Route shields: Mexican federal highways (`MEX 15`) need their own shield or a generic one.
- Extend the overview south to cover Mexico (Natural Earth admin-1 states, roads, cities).
- Terrain from Mapterhorn; no overlays.

**Done in 0.8.0 (11 November 2026)**, except Mexican-style addresses: see [sources](sources-and-architecture.md#mexico-080) and [validation](validation.md#080-mexico). Search finds house numbers only when they come first ("222 paseo de la reforma", not "reforma 222"); reading a trailing number would also turn "route 66" into an address search, so it needs its own rule. Mexican state routes (`SON 15`) use the plain marker.

### 2. Tile build for Canada and Alaska

- Build Shortbread tiles from Geofabrik's raw extracts, one file per province or territory and one for Alaska clipped west of the 180th meridian. Candidate tools: tilemaker with the Shortbread project's tilemaker configuration (what Geofabrik uses), or Planetiler (already pinned for the overlays) if it can produce the same layers. Check the licences first (see below).
- Validate each file with the core's existing Shortbread layer and coverage checks, so the app needs no special case for self-built packages.
- Publish as release files on GitHub, like the `overlays` release. GitHub's limit is 2 GiB per file; the largest provinces should fit, but this is to be measured.
- Decide a rebuild schedule (for example monthly). Update checks already compare server dates, so a rebuilt file shows as newer.
- Add Inuktitut syllabics to the bundled glyphs.

**Done for 0.9.0 (11 November 2026).** Built with VersaTiles' Planetiler profile (Ian's choice) and published in the [`regions` release](https://github.com/ianrastall/offline-north-american-maps/releases/tag/regions). Nunavut is two regions split at 90° W because one file would be 3.0 GB, over GitHub's 2 GiB limit; Quebec is the largest single file (1.7 GB). Rebuilds are by hand for now. Mexico is also split by state (Ian, 11 November): the states' maps are cut from Geofabrik's Mexico package and their extracts from its raw extract.

### 3. Canada and Alaska in the app

- Catalog entries pointing at the published tiles; stations, addresses and outdoor places from Geofabrik's raw extract, as for US regions today.
- Search: French street words (rue, chemin, boulevard, route) and names in both languages where OSM has them.
- Route shields: provincial highways and the Trans-Canada Highway.
- Extend the overview north to cover Canada and Alaska.
- Region identifiers from ISO 3166-2 (`ca-on`, `mx`, `us-ak`), as the US regions use.

**Done for 0.9.0**, except provincial and Trans-Canada shields (plain markers) and Inuktitut syllabics (the bundled Noto Sans has none; most Nunavut places also have Latin names). French search words are in. The Regions window lists the United States, Canada and Mexico under their own headings.

### 4. Name

Rename the app and repository for North America (the name is Ian's choice). The installer keeps its AppId, so existing installs upgrade in place, as they did for the 0.6.0 rename. The data folder does not change. GitHub redirects the old repository address.

**Done in 0.10.0 (11 November 2026):** Offline North American Maps, repository `offline-north-american-maps` (Ian's choice of name). The install folder, data folder, Start identity and project names (`OfflineMaps.*`, `offline-maps-*`) are unchanged.

### 5. Android foundation

- **Stack decision, recorded before code.** Jupiter has no Android standard. Proposed: Kotlin with Jetpack Compose for the interface, MapLibre Native for Android for the map and the existing Rust core through UniFFI or JNI. Record it in `_standards/` or in this project's exceptions.
- **Renderer proof for Android**, as the [renderer proof](renderer-proof.md) did for Windows: MapLibre Native reading the same MBTiles and styles, compared with the existing map page (`web/`) inside Android's WebView. Contour lines are the main difference: maplibre-contour runs only in the browser engine, so MapLibre Native would need contour tiles built ahead of time. Hillshading works in both.
- **Core port.** Build for `aarch64-linux-android` (and `x86_64` for the emulator). The core's HTTPS client uses SChannel, which is Windows-only; Android needs rustls or the platform's own client. Everything else (downloads, verification, store, tile merging, search, extras, terrain, saved places) is platform-neutral.
- **GPS.** Show the current position and heading, follow it, and save it as a place. Satellite positioning works without data; the first fix is slower without network assistance.
- **Distribution.** A signed APK on the GitHub releases; F-Droid later if wanted. Google Play is not required. The Windows installer rule in the workspace instructions does not apply to Android; the APK's install, upgrade with data kept, and uninstall are verified on a real phone.

### 6. Desktop to phone

- The desktop exports chosen regions (map, search index, extras, terrain, overlays) as a bundle the phone can import: over USB to the phone's storage, or on an SD card. No internet is involved.
- The phone verifies each file with the same SHA-256 and structural checks before activating it, and keeps the previous working copy until then, as the desktop does.
- Saved places travel both ways.

### 7. Phone downloads directly (optional)

Downloading on the phone when it is online, with a Wi-Fi-only setting and a storage check. Reading the raw extracts for stations and addresses is slow and drains the battery on a phone, so the phone either skips it, uses files prepared on the desktop, or downloads prebuilt extras (which would mean publishing OSM-derived files; see licensing).

### 8. Later

- **Routing** on both platforms. A separate decision: it needs a routing graph per region (a large derived file) and an engine such as Valhalla, GraphHopper or OSRM.
- **Overlays outside the US.** Canada: the Canadian Protected and Conserved Areas Database and provincial flood maps (coverage and licences vary by province). Mexico: protected areas from CONANP. Each source's licence is checked before use.

## Open decisions for Ian

1. The new name: decided 11 November 2026, Offline North American Maps.
2. Publishing self-built OpenStreetMap tiles (required for Canada and Alaska; see licensing): accepted 11 November 2026, in the `regions` release.
3. Whether the first Android version receives maps only from the desktop, or also downloads directly.
4. Android distribution: GitHub APK only, or F-Droid too.
5. Whether overlays outside the US are wanted.

## Licensing

- **OpenStreetMap (ODbL 1.0).** Publishing tiles built from OSM data makes this project a distributor of an ODbL-derived database. That is allowed with attribution ("© OpenStreetMap contributors") and with the tiles offered under ODbL. It reverses the current rule that map data stays out of this project's releases ([sources](sources-and-architecture.md#licensing-obligations)). Prebuilt extras for phones would be the same.
- **Tile build tools.** On 10 November 2026 GitHub reported no licence for `shortbread-tiles/shortbread-tilemaker` and "other" for `systemed/tilemaker`. Both are build tools, not shipped, but their terms are confirmed before use.
- **MapLibre Native** is BSD-2-Clause; its notice ships with the Android app.
- **Terrain outside the US** comes from Copernicus GLO-30, already credited in the app.
- **Overlay sources outside the US** are checked one by one; Canadian federal data is usually under the Open Government Licence – Canada, but provincial terms vary.
