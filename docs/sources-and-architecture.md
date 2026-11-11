# Sources and architecture

Checked 27 October 2026. Source pages can change; recheck before release.

## Built-in overview (0.7.0)

Natural Earth 1:10m (https://www.naturalearthdata.com/, downloaded from naciscdn.org), public domain: no permission or credit is required. `tools/overview/build_overview.py` reads admin-1 states (for region outlines; Puerto Rico and the Virgin Islands from their admin-0 units), land boundary lines, roads (US Interstate, federal and state levels), populated places, ocean, lakes and rivers (with the North America supplements), clipped to 180°W–48°W, 8°N–78°N; tags each piece with the region it lies in, splitting lines and areas at state borders; and tiles them with Planetiler (`tools/overview/overview.yml`) at zoom 0–7, using Natural Earth's own `min_zoom` per feature. The result is committed and installed in the program folder (`Overview\us-overview.mbtiles`).

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
- Search is a derived artifact built from the package's own tiles at preparation time (`search.sqlite`, about 98 MB for Illinois, SQLite FTS5), installed beside the package. Saved places are user data at the store root, independent of any package. See stage 5 in [validation](validation.md) for what the Shortbread package cannot provide (fuel stations, addresses, counties).
- Fuel and charging stations come from the raw `.osm.pbf` (about 363 MB for Illinois). The core's own PBF reader extracts them into a per-region extras database (about 2 MB) with its own source record and update cycle, and the raw file is deleted afterwards. Map and search use the extras when present and work without them. See stage 6 in [validation](validation.md).
- Addresses (about 843,000 in Illinois) come from the same raw extract into the extras database (about 35 MB with them). A query starting with a house number searches them first. See stage 7 in [validation](validation.md).
- The WinUI shell reaches the core through a protobuf C ABI (`proto/offline_maps.proto`, `crates/offline-maps-ffi`); map tiles have their own export. The map page in WebView2 gets tiles and stations through the shell's request handler, never over a socket. See stage 8 in [validation](validation.md).

## Regions (0.2.0)

- The catalog (`crates/offline-maps-core/src/regions_data.rs`) is generated by `tools/make-regions.py` from Geofabrik's region index (`https://download.geofabrik.de/index-v1.json`) and HEAD requests for each file's size. It lists 52 regions: the states except Alaska, the District of Columbia, Puerto Rico and the US Virgin Islands, with IDs from ISO 3166-2 (`us-il`). Mexico was added in 0.8.0 (below). Every one has a Shortbread package (checked 8 November 2026; all lower 48 plus DC about 14 GB).
- Alaska is left out: its outline crosses the 180th meridian, so its Shortbread package spans the whole width of the world (29.6 GB on 8 November 2026).
- Each region is checked as Illinois was: the package must lie within its outline's bounding box widened by half a degree and cover five points inside it. Geofabrik's package bounds equal the outline's bounding box.
- Each region keeps its own folder in the store (`<root>/us-in/…`) with its package, search index and extras, and its own update cycle. Removing a region deletes that folder; saved places are kept.
- The map page sees one source covering the box around all installed regions. For each tile, the core reads every installed package whose bounds touch it. Where more than one has the tile (a border), same-named layers are joined (`merge.rs`): key and value tables without repeats, tags renumbered, identical features kept once. Each extract includes whole roads and relations that cross its outline, so some of a neighbouring state shows even when only one region is installed.
- Search runs over every installed region's index and extras and ranks the results together. Places with the same name in different regions (Springfield, Illinois and Missouri) are kept apart; the same place in two regions' data is listed once.

## Mexico (0.8.0)

- Geofabrik publishes Mexico as one region, with a Shortbread package (1.92 GB on 11 November 2026) and a raw extract (648 MB); it does not split it into states. The catalog lists it with the ID `mx` (ISO 3166-1), so the catalog has 53 regions. `tools/make-regions.py` takes its file addresses from `north-america/` rather than `north-america/us/`.
- Its package covers Geofabrik's whole outline box (132.5°W–85.5°W, 9.5°N–32.7°N, which takes in the Revillagigedo Islands and the open Pacific), and its tiles hold everything in that box: northern Central America (Guatemala, Belize, El Salvador, Honduras, most of Nicaragua) and the US border areas (San Diego, Tucson, El Paso, San Antonio, Houston) as well as Mexico. Search builds from the same tiles, so those places are searchable too. Stations, addresses and outdoor places come from the raw extract, which follows Mexico's outline.
- Terrain is fetched for the map's box, as for every region: 4.59 GB from Mapterhorn (Copernicus GLO-30; Mapterhorn lists no Mexican source). A box drawn tightly around Mexico's land and islands measured 4.34 GB, so the ocean adds little and no special case was made.
- Flood zones and public lands are US data (FEMA, USGS); none is published for Mexico, and the Regions window says so.
- Search folds accents when it compares names (the full-text index already ignored them), knows Spanish street abbreviations (Av., Calz., Carr., Prol., Priv., Col., Blvd.) and finds stations by Spanish words (gasolinera, farmacia, bomberos, agua potable, baños). House numbers must come first in a query ("222 paseo de la reforma"); Mexican addresses are usually written with the number after the street, which is not yet understood.
- Highway shields: `MEX` federal routes get their own white shield with a black cap; Mexican state routes use the plain marker.
- The overview covers Mexico's 32 states (outlines, borders and names; Mexico City under its current name), its federal highways, 189 cities and its borders with Guatemala and Belize. Every Mexican feature carries the region `mx`.

## Regions built for the app (from 0.9.0)

Geofabrik publishes Mexico only as a whole and has no Shortbread packages for Canada or a usable one for Alaska. Those regions' files are made with `tools/regions-build/` and published in this repository's [`regions` release](https://github.com/ianrastall/offline-us-maps/releases/tag/regions); the catalog (`tools/make-regions.py --built`, from `tools/regions-build/manifest.py`) points the app at them. The files are OpenStreetMap data, so the release offers them under the ODbL with attribution, as Ian decided on 11 November 2026.

- **Mexican states** (`tools/regions-build/mexico_states.py`, pyosmium 4.3 and shapely): the 32 states are the OpenStreetMap boundaries with admin_level 4 and an ISO 3166-2 code MX-*, read from Geofabrik's Mexico extract (islands included) and widened by 0.02° (about 2 km) so border features are in both neighbours. Each state's map is the tiles of Geofabrik's Mexico package that touch the widened outline, at every zoom, unchanged (5.6 MB Aguascalientes to 59 MB Chihuahua, 830 MB in all). Each state's raw extract holds the nodes inside, the ways with a node inside (first, middle or last) with all their nodes, and the relations with a member inside; its header carries the state's box and the source's replication date (4–50 MB, 600 MB in all). ID trackers reserve about 1.7 GB each for node IDs, so the cut runs eight states per pass (about 12 minutes a pass). The app installs and searches these exactly like Geofabrik's files. "All of Mexico" stays in the catalog as one region.
- **Canada and Alaska** (`tools/regions-build/build_shortbread.py`): built from Geofabrik's raw extracts with a Planetiler Shortbread profile; Alaska is clipped at the 180th meridian (the Near Islands are left out). Stations and addresses come from Geofabrik's extracts directly. Planetiler's bundled sample profile was tried on Rhode Island against Geofabrik's package: it writes no boundary lines and about a third fewer addresses, place labels and river names. VersaTiles' Planetiler profile (Apache-2.0) fixes those; which profile to use is with Ian.
- Overview: every state and province carries its own ID (`mx-que`, `ca-on`); the map's substring match means the whole-country `mx` also hides every Mexican state's overview.

## Terrain (0.3.0)

- Source: Mapterhorn's planet archive, `https://download.mapterhorn.com/planet.pmtiles` (PMTiles v3, 355.6 GB, zoom 0–12; Terrarium-encoded elevation as 512-pixel WebP). For the United States its sources are the USGS 3D Elevation Program at 1 m and 10 m (public domain) and Copernicus GLO-30 at 30 m (Copernicus licence: free use and redistribution with attribution). Source list: `https://download.mapterhorn.com/attribution.json`. Checked 8 November 2026.
- Mapterhorn documents extracting areas from the planet file with range requests (`pmtiles extract --bbox`) and states no download limits. The core does the same itself (`pmtiles.rs`, `terrain.rs`): it reads the archive's header and directories (about 0.2 MB per state), then fetches the region's tiles in joined ranges (gaps up to 512 KB, at most 16 MB per request), every request after the first with `If-Range` on the archive's ETag. A file replaced mid-download is detected and the next attempt starts again; a cancelled download resumes.
- Extent: the installed map's bounds (the region's outline box), zoom 0–12, about 15 m per pixel at 40° N. Zoom 13 would add about four times as much (about 1.1 GB more for Illinois). Illinois: 5,309 tiles, 366 MB. All 52 regions: about 23 GB; Texas 1.9 GB, California 1.6 GB, Montana 1.4 GB.
- Storage: `<root>/<region>/terrain/versions/<v>/terrain.mbtiles` with a record and an `active.json` pointer, like the extras. Removing a region deletes it.
- Rendering: the map page asks for `terrain/{z}/{x}/{y}.webp`; the shell answers from the core. MapLibre draws hillshading (a `raster-dem` source); maplibre-contour 0.1.1 (BSD-3-Clause, pinned in `proof/renderer`) computes contour lines in a web worker from the same tiles: in feet (200/1000 at zoom 10 down to 10/50 at zoom 16) or metres with the distance setting. Both sit under water, roads and labels.

## Flood zones and public lands (0.5.0)

Both are US Government works in the public domain. The app does not convert GIS files: the maintainer builds one overlay file per region and publishes it with the GitHub releases (release tag `overlays`, files `<region>-overlays.mbtiles`); the app downloads it like the other region files.

- Flood zones: FEMA National Flood Hazard Layer, the state file from FEMA's Map Service Center. `POST https://msc.fema.gov/portal/advanceSearch` (form fields `selstate`, `selcounty`, `selcommunity`, `method=search`) lists `EFFECTIVE.NFHL_STATE_DATA` products; the file is `https://msc.fema.gov/portal/downloadProduct?productTypeID=NFHL&productSubTypeID=NFHL_STATE_DATA&productID=<product>` (Illinois on 8 November 2026: `NFHL_17_20261027`, 314 MB, effective 21 July 2026, an Esri file geodatabase). From layer `S_FLD_HAZ_AR` the build keeps floodways, 1% annual-chance zones (`SFHA_TF = T`: A, AE, AH, AO, VE), 0.2% annual-chance and shallow 1% zones, and areas with reduced risk behind levees; it leaves out minimal-hazard and undetermined areas and open water. FEMA updates these files continually; the effective date travels with the overlay file. Where the search lists no state file (Alabama, Idaho, Louisiana, Washington and the US Virgin Islands on 8 November 2026), the same layer comes from FEMA's public NFHL map service (`https://hazards.fema.gov/arcgis/rest/services/public/NFHL/MapServer/28/query`): the IDs of matching areas are listed in half-degree cells, then the areas are read by ID, 100 at a time, with coordinates to six decimals. Queries returning a whole cell's shapes failed on the server (HTTP 500); a batch that fails is split, and a single area still too large (one in Alabama) is read simplified to 0.5 m, below what a zoom 14 tile shows. Such files record the build date, marked "(map service)", as the flood data date.
- Public lands: USGS PAD-US 4.1. ScienceBase, where the bulk files are, now requires signing in to download, so the build reads the public "PAD-US Public Access" feature service instead (`https://services.arcgis.com/v01gqwM5QqNysAAi/arcgis/rest/services/PADUS_Public_Access/FeatureServer/0`, ArcGIS Online item `c91a5655a1be428daeb778888e60db24`), paged 2,000 at a time over the region's box, owned land (`Fee`) and easements only. Proclamation boundaries (which enclose private land) and designations (which overlap owned land) are left out. Access is shown as open, restricted, closed or unknown.
- Build: `tools/layers/build_overlays.py` (Python with pyogrio/GDAL 3.12 and geopandas, pinned in `tools/layers/requirements.txt`) writes GeoPackages in WGS 84; Planetiler 0.10.2 (Apache-2.0, checksum pinned; a build tool, not shipped) tiles them with `tools/layers/overlays.yml` into zoom 5–14 layers `flood_zones`, `public_lands` and `public_land_labels`, and the build stamps the FEMA product, its effective date, the PAD-US version and the build time into the file's metadata. Illinois: 68,953 flood areas and 14,268 public-land areas, 70 MB, built in about two minutes after the 314 MB FEMA download.
- In the app: per-region files in `<root>/<region>/overlays/`, served to the map at `overlays/{z}/{x}/{y}.pbf` and merged across neighbouring regions like the map tiles; drawn translucent under water, roads and labels.

## Licensing obligations

- OpenStreetMap data (ODbL 1.0): show "© OpenStreetMap contributors" on the map and keep license details with packages. Distributing tiles or a derived database (for example committing them to a public repository or bundling them in a release) triggers share-alike obligations, so map data stays out of git and out of the installer; the app downloads it from Geofabrik.
- MapLibre GL JS: BSD-3-Clause. VersaTiles Style: MIT. VersaTiles sprites: CC0. Noto Sans: OFL 1.1. Ship their license texts with the app.
- Terrain (when downloaded): credit Mapterhorn, the USGS and Copernicus on the map (short form, with the full Copernicus notice as a tooltip) and in Map information and the third-party notices: "© DLR e.V. 2010–2014 and © Airbus Defence and Space GmbH 2014–2018 provided under COPERNICUS by the European Union and ESA; all rights reserved". USGS data is public domain. Terrain stays out of git and the installer; the app downloads it.
- Flood zones and public lands: public domain (FEMA, USGS); credited on the map and in Map information. The overlay files may be redistributed, which is what allows publishing them prebuilt.
- maplibre-contour: BSD-3-Clause; its license text ships with the app and in the third-party notices.
- Geofabrik labels the Shortbread package experimental. Its file is rebuilt about weekly; record ETag and Last-Modified per download.

## Package storage contract

Keep mutable settings and packages outside the installation directory. Persist versioned metadata including region ID, source URL, license, source date, downloaded date, format, byte length and SHA-256. Download to staging storage with explicit size/disk limits. Verify source version when resuming: never append to a changed latest file. Validate format and renderer readability before atomically activating a version. Keep the last known working map until replacement passes checks. Never treat transfer completion as map readiness.

Implemented in the core: bounded streaming with cancellation, read-only MBTiles access, SHA-256 and structural verification, region coverage and Shortbread layer checks, the versioned package record (stage 3), and the HTTPS transport, staged store, ETag-checked resume, older-data guard and atomic activation (stage 4 in [validation](validation.md)). The region catalog is built in (see below).

Geofabrik's download proxies cache responses for up to a week; requests must send `Cache-Control: no-cache` or they may report and serve an older build. See stage 4 in [validation](validation.md).
