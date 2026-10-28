# Implementation and validation

## Stage 1: project and core foundations

Implemented: Illinois source references, attribution constants, finite/range-checked coordinates, package readiness states and bounded streaming to caller-owned staging storage. Unit tests cover multibuffer transfers, unknown lengths, truncation, empty bodies, size limits, cancellation, write failures and readiness.

This is not an HTTP download or an offline map demonstration. The caller must supply a response reader and staging writer and validate package format separately.

Verified on Windows x64 on 27 October 2026 with Rust 1.99.1: all 8 unit tests passed; Clippy passed with warnings denied. Formatting checked with rustfmt. No dataset download or installed-app verification has been performed.

## Next stages

1. Inspect Illinois vector package metadata and license; prove local rendering with all assets bundled. Measure storage, preparation time and usability at road/street zooms. Decide renderer based on this proof.
2. Implement real download transport with finite timeouts, cancellation, size checks, checksum verification and version-safe resume; preserve previous packages through failures.
3. Build offline place search and saved places. Verify Illinois boundary behavior and show package coverage instead of silently requesting remote data.
4. Integrate Rust through the canonical ABI into the Jupiter WinUI workspace. Validate keyboard, resizing, accessibility, light/dark and accent settings.
5. Produce the installer through the normal release command. Install and launch from Start Menu. Test an upgrade and uninstall while preserving user data.
6. Launch with networking disabled and verify map rendering, labels, search and saved places from a clean process. This is the decisive first-workflow acceptance test.
