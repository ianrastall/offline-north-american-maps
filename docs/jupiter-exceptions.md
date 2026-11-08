# Jupiter conformance

Target: canonical WinUI 3/C# + Rust core architecture and Jupiter style standard 1.1.

1. **Embedded map surface (selected 8 November 2026).** The map itself is rendered by MapLibre GL JS inside WebView2, drawing local Shortbread vector tiles with bundled style, glyphs and sprites. This is a content-rendering exception to native-control defaults, following the Lichess Local precedent. Basis: [renderer proof](renderer-proof.md). Constraints:
   - The WinUI shell owns the menu bar, regions/search pane, status bar, Settings, pickers, window lifecycle, installer identity and the shared Jupiter.UI theme/accent controls. Map controls inside the web view receive Jupiter.UI theme and accent values at runtime; no separate interface palette is maintained. Map colors are content styling.
   - All web content is local: assets through a WebView2 virtual host mapping, tiles through request interception backed by the Rust core. No TCP listener, no remote origins, and a same-origin Content Security Policy.
   - The Rust core remains the authority for packages, validation, search and saved places. The web view only renders and reports map interactions.

No GUI has been built or delivered yet. The future GUI must import shared Jupiter.UI resources and its error-mode build check, avoid NumberBox, and ship through an installer with Start Menu verification.
