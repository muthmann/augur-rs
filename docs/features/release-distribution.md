# Release Distribution

## Summary

AugurRS tagged releases now produce four downloadable artifacts:

- `augur-macos.zip` with the macOS CLI binaries and supporting files
- `AugurRS.dmg` with an unsigned macOS GUI installer
- `augur-linux.tar.gz` with Linux binaries and supporting files
- `augur-windows.zip` with Windows binaries and supporting files

This keeps tagged releases aligned with the repository's stated platform support while preserving source builds as the path for optional native features such as HDF5 replay.

## macOS DMG Packaging

The macOS release job assembles `AugurRS.app`, stages it with an `Applications` symlink, and creates a distributable `AugurRS.dmg`. The bundle script derives `Contents/Resources/AugurRS.icns` from `assets/logo.png` at packaging time, copies the bundled executable as `Contents/MacOS/AugurRS`, and writes matching bundle metadata into `Info.plist`, so Finder, the Applications folder, and macOS crash reports all use the same product name and icon.

## Cross-Platform Release Archives

Linux and Windows tagged releases now build the same `augur` and `AugurRS` binaries that portability CI already checks, then package each platform with:

- the platform binaries
- `README.md`
- `LICENSE`
- `CONTRIBUTING.md`
- `CHANGELOG.md`
- `examples/augur.toml`

That keeps the downloaded archives self-describing without requiring a separate docs lookup.

## Versioning Workflow

Workspace versioning is managed through `release.toml` and `cargo-release`.

Typical release flow:

```bash
cargo install cargo-release
cargo release patch --dry-run
cargo release patch --execute
```

Before running the release command, update `CHANGELOG.md` with the user-facing changes for the next version.

## Limits

- Tagged binaries still exclude the optional `hdf5` feature, so HDF5 replay remains a source-build-only setup.
- The macOS `.dmg` remains unsigned, so Gatekeeper can still prompt on first launch.
