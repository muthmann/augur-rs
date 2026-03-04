# Release Notes

## Current State

AugurRS currently ships in three layers:

- source builds through Cargo
- `augur-macos.zip` for CLI-first macOS distribution
- `AugurGUI.app.zip` for an unsigned macOS GUI app bundle

## What The Release Workflow Publishes

Tagged releases build:

- `augur`
- `augur-gui`
- `augur-macos.zip`
- `AugurGUI.app.zip`

The archive release includes the binaries, top-level docs, and `examples/augur.toml`.

## What Is Still Missing

- code signing
- notarization
- installer-based distribution

That means the `.app` bundle is useful for testing and sharing, but it is not yet polished like a fully notarized macOS desktop release.
