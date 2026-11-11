# Offline US Maps project brief

27 October 2026

## Confirmed purpose

Ian wants road maps for getting around during an emergency, available offline. Satellite imagery is not the main requirement. Illinois is the first region; from 0.2.0 any US state except Alaska can be added, alongside DC, Puerto Rico and the US Virgin Islands. The original Google satellite downloader reference prompted the project, but does not define its data provider or visual design.

## First useful workflow

1. While online, choose Illinois and see coverage, download size, data date and required disk space.
2. Download and prepare a map package, with progress, cancellation and a recoverable failure state.
3. Open the installed app with networking disabled. Pan and zoom a labeled road map showing towns, roads and useful landmarks.
4. Find a town or saved location offline. Retain data source, coverage and last update details locally.
5. Update maps while online without replacing the last working package until the new package validates.

Offline search is part of the intended first workflow. A downloadable file alone does not complete it. Turn-by-turn routing, GPS integration, live closures, traffic and mobile applications are separate decisions (an Android app with GPS was agreed on 10 November 2026; see below). Windows 11 x64 is the initial platform under Jupiter conventions. A desktop app can help prepare for travel; use on a phone or a GPS device would require an additional delivery target.

## Interface direction

Map workspace with a traditional menu bar, compact map controls, a regions/search pane and a status bar showing current package and data date. Settings includes the shared Jupiter theme and accent controls. Display source attribution on the map and include license information with exported data. Use numeric TextBox, ComboBox or Slider controls; never NumberBox.

## Publication (decided 8 November 2026)

- Published on GitHub after the three new layers (outdoor places, flood zones, public lands) as **Offline US Maps** (repository `offline-us-maps`), covering the US regions in the catalog. Anyone with the installer can use it locally; no account or service is involved.
- Licence: public domain, as The Unlicense, for the app's own code. Bundled components keep their own licences and notices (MapLibre GL JS and maplibre-contour BSD-3-Clause, VersaTiles style MIT, sprites CC0, Noto Sans OFL, Rust crates MIT/Apache-2.0). Map data is downloaded by the user under its own terms.
- Jupiter.UI is published as its own public repository under the same licence, so the source can be built by others.
- World coverage was considered and set aside: Geofabrik publishes Shortbread packages for 356 of its 554 regions (checked 8 November 2026; none for Germany, Japan, Brazil, Ontario or Britain, for example), and search abbreviations, address parsing and route shields are US-specific. The region catalog itself is not US-specific.

## North America and Android (direction agreed 10 November 2026)

Ian reviewed the purpose: a survival tool for a serious national or world crisis, when electricity is available but internet is not. Two conclusions follow, and they replace the US-only scope above and the brief's earlier note that mobile applications are a separate decision.

- **North America, not the US alone.** People in a crisis cross borders, and Alaska is reachable by land only through Canada. Coverage extends to Canada and Mexico, and Alaska is added.
- **A phone app as well as the desktop app.** An Android phone is the more useful tool in a crisis, mainly because its GPS works without internet. The desktop is the "guy in the chair": it downloads over a good connection, does the heavy processing, shows everything on a large screen and copies prepared regions to the phone over USB, which needs no internet. The phone carries the regions and shows where you are.

Phone capacity is not the limit: the map engine reads only the tiles on screen, and Illinois with every optional download takes about 0.9 GB. What does not port is the WinUI shell and the WebView2 map page; the Rust core does.

The stages, facts checked and open decisions are in [North America and Android plan](north-america-and-android.md). No stage has started. The first step is the networking-disabled acceptance test of the existing app.

## Acceptance conditions

- Illinois roads and labels render from local storage after a fresh launch with networking disabled.
- Search and saved places do not require geocoding services or a cached online session.
- No remote glyphs, fonts, sprites, style documents or web assets are required to render the map.
- Interrupted downloads, storage exhaustion and failed updates leave the previous verified map usable.
- Map metadata distinguishes source data date, download date and preparation date.
- No claim that offline maps include current emergency conditions or road closures.
- A normal release produces the Windows installer; install, upgrade, Start Menu launch and uninstall behavior are verified on Ian's machine. Uninstall preserves settings and user-created data, including saved places and map packages.
