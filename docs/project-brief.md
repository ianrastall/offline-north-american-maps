# Offline Maps project brief

27 October 2026

## Confirmed purpose

Ian wants road maps for getting around during an emergency, available offline. Satellite imagery is not the main requirement. Illinois is the first region; from 0.2.0 any US state except Alaska can be added, alongside DC, Puerto Rico and the US Virgin Islands. The original Google satellite downloader reference prompted the project, but does not define its data provider or visual design.

## First useful workflow

1. While online, choose Illinois and see coverage, download size, data date and required disk space.
2. Download and prepare a map package, with progress, cancellation and a recoverable failure state.
3. Open the installed app with networking disabled. Pan and zoom a labeled road map showing towns, roads and useful landmarks.
4. Find a town or saved location offline. Retain data source, coverage and last update details locally.
5. Update maps while online without replacing the last working package until the new package validates.

Offline search is part of the intended first workflow. A downloadable file alone does not complete it. Turn-by-turn routing, GPS integration, live closures, traffic and mobile applications are separate decisions. Windows 11 x64 is the initial platform under Jupiter conventions. A desktop app can help prepare for travel; use on a phone or a GPS device would require an additional delivery target.

## Interface direction

Map workspace with a traditional menu bar, compact map controls, a regions/search pane and a status bar showing current package and data date. Settings includes the shared Jupiter theme and accent controls. Display source attribution on the map and include license information with exported data. Use numeric TextBox, ComboBox or Slider controls; never NumberBox.

## Acceptance conditions

- Illinois roads and labels render from local storage after a fresh launch with networking disabled.
- Search and saved places do not require geocoding services or a cached online session.
- No remote glyphs, fonts, sprites, style documents or web assets are required to render the map.
- Interrupted downloads, storage exhaustion and failed updates leave the previous verified map usable.
- Map metadata distinguishes source data date, download date and preparation date.
- No claim that offline maps include current emergency conditions or road closures.
- A normal release produces the Windows installer; install, upgrade, Start Menu launch and uninstall behavior are verified on Ian's machine. Uninstall preserves settings and user-created data, including saved places and map packages.
