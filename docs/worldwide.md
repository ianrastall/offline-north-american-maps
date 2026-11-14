# Worldwide plan

14 November 2026. Proposed at Ian's request. Ian started stage W1 the same day and chose the name Offline Maps; the other stages are not yet agreed. It extends the [North America and Android plan](north-america-and-android.md) and depends on its stage 5 (prepared bundles on the release page).

## Purpose

The built-in overview is a small whole-area base map, and regions are filled in by downloading. The same model can cover the whole planet: the overview shows every part of the world, and anyone downloads and fills in the regions they need, where the data allows.

The two editions keep their jobs:

| | Windows (the "guy in the chair") | Android phone |
| --- | --- | --- |
| Coverage | Any regions, as many as the map folder holds (since 0.11.0 the folder can be on any drive) | A home region and its neighbours, received from the desktop or downloaded |
| Detail | Everything: map, search, stations and addresses, terrain, overlays where they exist | The same files, chosen to fit the phone's storage |

## Facts checked 14 November 2026

**Geofabrik's regions** (`https://download.geofabrik.de/index-v1.json`): 554 regions, 523 with no subregions. A few of those overlap others as groupings (`dach`, `alps`, `britain-and-ireland`, `us-west`). The index includes each region's outline.

**Geofabrik's Shortbread packages** (HEAD request for `<region>-shortbread-1.0.mbtiles` for each of the 523 regions with no subregions):

| Area | Regions | With a package | Package total | Without a package |
| --- | --- | --- | --- | --- |
| Africa | 56 | 55 | 12.9 GB | South Africa |
| Antarctica | 1 | 1 | 6.9 GB | none |
| Asia | 89 | 55 | 20.2 GB | China's 33 provinces, cities and special administrative regions, and `sea` (Southeast Asia as one grouping) |
| Australia and Oceania | 36 | 22 | 38.2 GB | Australia's states and territories (Australia as a whole has one, 2.84 GB) and small island territories |
| Central America | 11 | 0 | none | all, including Cuba, Haiti and the Dominican Republic, Guatemala and Panama |
| Europe | 222 | 133 | 37.8 GB | 89, including England's counties, Germany's administrative districts, the Czech regions, Great Britain and `dach` |
| North America | 82 | 54 | 49.2 GB | Canada's provinces and territories (already built here), California's two halves and the US groupings |
| Russia | 10 | 10 | 74.0 GB | none |
| South America | 16 | 16 | 9.4 GB | none |
| Total | 523 | 346 | 248.5 GB | 177 |

The package total is inflated by regions that cross the 180th meridian. Geofabrik builds those as if they spanned the world, as it does for Alaska (29.6 GB): Russia's Far Eastern Federal District 58.4 GB, American Oceania 15.3 GB, Kiribati 7.3 GB, Antarctica 6.9 GB, New Zealand 6.6 GB, Fiji 4.8 GB, Tuvalu 3.1 GB. Clipped at the meridian, as Alaska is here, they are far smaller.

**Raw extracts** (`.osm.pbf`, for stations, addresses and outdoor places, and for building tiles): Europe 35.4 GB, Asia 16.5 GB, Africa 8.0 GB, Russia 4.2 GB, South America 4.2 GB, Australia and Oceania 1.6 GB, Central America 0.8 GB, Antarctica 0.03 GB. The whole planet is 95.8 GB (`planet.openstreetmap.org`).

**A ready-made planet in the same tile schema.** VersaTiles publishes the whole planet as one Shortbread tile set (`download.versatiles.org/osm-vector/osm-vector.versatiles`, 66.6 GB). It is built with the same Planetiler profile this project already uses for Canada, Alaska and the Mexican states, and the app's style is derived from VersaTiles' style. Cutting regions from it would give every region consistent tiles from one download. Not yet checked: whether VersaTiles' tool cuts by outline or only by box, whether its output passes the core's layer and coverage checks, and how current the file is.

**Terrain.** Mapterhorn covers the whole planet. Its [attribution page](https://mapterhorn.com/attribution/), readable this time (it was not on 11 November), lists the sources: national 1–10 m elevation models for the US, Canada, Japan, New Zealand, Taiwan, Israel, Rwanda, Australia (parts) and most of Europe, and Copernicus GLO-30 everywhere else. The licences are mostly CC BY 4.0, open government licences, CC0 or public domain. Japan's (国土地理院コンテンツ利用規約, used under a survey-law approval number) is a terms-of-use document, not a standard open licence.

**Glyphs.** The font set already downloaded (VersaTiles Noto Sans, 256 ranges per face) includes Arabic, Devanagari, Thai, Hangul and CJK. The app bundles 23 ranges (6.8 MB). All ranges for the regular and bold faces would be about 76 MB. MapLibre GL JS draws Arabic and Hebrew right to left only with its RTL text plugin. It does not shape Devanagari, Bengali, Tamil, Thai or other complex scripts properly, so labels in those scripts show with broken letter forms. That is a limit of the map engine, not of the data.

**Overview.** The current overview is 4.2 MB: Natural Earth 1:10m at zoom 0–7, clipped to 180°W–48°W, 8°N–84°N. Natural Earth covers the whole world with the same layers. The world overview built for W1 is 13.5 MB.

**Hosting.** GitHub allows up to 1,000 files per release and 2 GiB per file, with no stated limit on total size or bandwidth. Several hundred regions with a map and an extras file each would need one release per continent. Regions over 2 GiB are split, as Nunavut is.

## What carries over and what does not

| Part | Worldwide |
| --- | --- |
| Map packages, verification, store, updates, the map folder | Unchanged |
| Overview | Rebuilt for the whole world; the regions' outlines come from Geofabrik's index |
| Stations, addresses and outdoor places | The core reads any raw extract; tags vary by country, and address formats differ (Japan numbers blocks, not streets) |
| Search | Works, but is tuned for English, Spanish and French. CJK text needs a different full-text tokenizer: SQLite's `unicode61` does not split Chinese or Japanese into words |
| Labels | Latin, Greek and Cyrillic now; Arabic and Hebrew need the RTL plugin; CJK and Hangul need bigger font bundles; complex scripts stay imperfect (see above) |
| Route shields | US, Canadian and Mexican styles; plain markers elsewhere |
| Terrain | Works everywhere; attribution must follow the region |
| Flood zones and public lands | US only |
| Regions window | 100 regions listed under three countries. Several hundred need continent and country levels, with search |
| Name | "North American" no longer fits |

## Stages

### W1. World overview and regions to fill in

- Build the overview for the whole world from Natural Earth, with every catalog region's outline.
- On the map, a region with nothing installed is drawn as an outline; clicking it offers its download. The Regions window gains continent and country levels.
- Rename the app and repository (Ian's choice), keeping the AppId, install folder, data folder and Start identity, as the 0.6.0 and 0.10.0 renames did.
- No new region data yet; the catalog stays at 100 regions.

**Done in 0.12.0 (14 November 2026)**, except the Regions window's continent level: with every region in North America it would add a heading with one entry, so it moves to W2, which brings regions on other continents. The name is Offline Maps (Ian's choice), the app's first name, which already matched the install folder, project names and Start identity; the repository is `ianrastall/offline-maps`, and GitHub redirects the old addresses.

### W2. Catalog and builds for every region

**First part done in 0.13.0 (14 November 2026)**: the 286 regions Geofabrik publishes a Shortbread package for (outside North America, Greenland included), with their country and continent, the Regions window's continent filter and country groups, and their outlines in the overview; regions in other scripts are offered with a note (Ian's choice: offer them now). Left for the rest of W2: the regions with no package (China's provinces, Central America, Australia's states, England's counties and others, 177 in all), the seven that cross the 180th meridian (Fiji, Kiribati, New Zealand, Tuvalu, American Oceania, Antarctica, Russia's Far East) and stage 5's publishing.

- Generate the catalog from Geofabrik's index for all regions with no subregions, skipping the grouping regions. Use ISO 3166 identifiers where they exist (`de-by`, `jp`) and Geofabrik's otherwise.
- Choose one source for map tiles, after measuring a few countries both ways:
  - cut from the VersaTiles planet (one 66.6 GB download, one schema), or
  - Geofabrik's packages where they exist (346 regions), with the rest built from raw extracts with `tools/regions-build/` (177 regions; their extracts total 59.8 GB, less once the overlapping groupings are dropped).
- Clip every region that crosses the 180th meridian at it, as Alaska is.
- The Regions window gains continent and country levels (moved from W1).
- Outlines in the overview's `regions` layer for every new region, so each can be clicked to download.
- Split any region over 2 GiB.
- Publish through stage 5's maintainer build: maps, processed extras databases and a manifest with SHA-256, size and data date, one release per continent.

### W3. Latin-script regions in the app

Europe, Central and South America, Africa outside the Arabic-script countries, Oceania. Search gains street words for the main languages (German, Portuguese, Italian, Dutch, Polish and so on) and the accent folding that Spanish already uses.

### W4. Other scripts

- Bundle the full glyph ranges (about 76 MB more in the installer), or download a script's ranges with the first region that needs them.
- Add MapLibre's RTL text plugin for Arabic and Hebrew.
- Add a search tokenizer for CJK text (SQLite's `trigram` tokenizer, or an ICU-based one).
- Record that South and Southeast Asian scripts draw with broken letter forms until the map engine shapes them.

### W5. Terrain attribution by region

Show each installed region's terrain sources and licences in Map information and the credits line, from Mapterhorn's `attribution.json`. Read Japan's terms before offering terrain there.

### W6. Phone

No separate work: the Android stages (6–9 of the North America plan) take regions from the same catalog and bundles.

## Open decisions for Ian

1. The new name and repository name (W1): decided 14 November 2026, Offline Maps, repository `offline-maps`.
2. Order: W1 before or after the Android foundation (stage 6 of the North America plan). W1 is independent of it; W2 depends on stage 5.
3. The tile source for regions without a Geofabrik package: the VersaTiles planet, or builds from raw extracts (decided after measuring, W2).
4. Fonts: bundled in the installer, or downloaded per script (W4).
5. Whether regions whose labels cannot draw properly (complex scripts) are offered with a note, or held back: decided 14 November 2026, offered with a note.
6. Rebuild frequency for several hundred regions (stage 5 proposes monthly).

## Licensing

Nothing found blocks this plan; one terrain source needs its terms read.

- **OpenStreetMap (ODbL 1.0).** The same terms as now: attribution, and published tiles and extras offered under ODbL, as accepted for the `regions` release on 11 November 2026.
- **VersaTiles planet.** OpenStreetMap data under ODbL; VersaTiles' tools and style are MIT. Confirm both on their pages before relying on the planet file.
- **Natural Earth.** Public domain.
- **Noto Sans.** SIL OFL 1.1, already bundled in part.
- **MapLibre RTL text plugin.** Check its licence before bundling.
- **Terrain.** Each Mapterhorn source carries its own licence and credit. Most are CC BY 4.0, open government licences, CC0 or public domain, and Copernicus GLO-30 is under the Copernicus free and open licence, already credited. Japan's GSI terms are to be read before terrain is offered for Japan.
- **Overlays outside the US.** Each source is checked one by one, as the North America plan already says.
