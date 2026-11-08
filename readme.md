# Offline Maps

Jupiter project for downloading road maps before an emergency and using them without internet access. Working application name: **Offline Maps**. First region: **Illinois**.

Status: project started on 27 October 2026. Product brief, source assessment and a tested Rust package-transfer foundation exist. On 8 November 2026 a [renderer proof](docs/renderer-proof.md) rendered Geofabrik's Illinois vector tiles entirely from local files with MapLibre; MapLibre in WebView2 is the selected map surface. There is no app map viewer, HTTP downloader, search, routing engine or installed application yet.

- [Project brief](docs/project-brief.md)
- [Sources and technical decisions](docs/sources-and-architecture.md)
- [Renderer proof and decision](docs/renderer-proof.md)
- [Implementation stages and verification](docs/validation.md)
- [Jupiter conformance](docs/jupiter-exceptions.md)

## Development

Windows 11 x64, Rust 1.99.1 (pinned). From this directory:

```powershell
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

The renderer proof (Node 24) is in `proof/renderer`; see [renderer proof](docs/renderer-proof.md) for how to rerun it. Downloaded map data and assets go under `data/`, which is not committed.

The future WinUI 3/C# application must import Jupiter.UI and follow Jupiter style standard 1.1. Normal release builds must produce an installer with bundled prerequisites, a Start Menu entry, registered uninstall and upgrades preserving settings and downloaded maps. No GUI is delivered by this initial core milestone.
