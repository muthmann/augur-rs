# ADR 004: Cross-Platform Release Pipeline With macOS DMG Distribution

## Status

Superseded in part by [ADR 032](./032-installer-distribution-and-in-app-updates.md), which replaces
the artifact set and the release trigger described here. The `cargo-release` / `CHANGELOG.md`
conventions below still stand.

## Context

AugurRS already advertised macOS, Linux, and Windows support, but tagged releases only produced a macOS CLI archive and an unsigned GUI app zip. That left Linux and Windows users on source builds only, and macOS desktop users without a cleaner drag-to-Applications installer.

The repository also lacked a standard release operator workflow for bumping the shared workspace version and tagging a release consistently.

## Decision

Adopt a release pipeline with three platform-specific build jobs plus workspace-level `cargo-release` configuration:

- macOS builds `augur` / `augur-gui`, assembles `AugurRS.app`, stages it with an `Applications` symlink, creates `AugurRS.dmg`, and also publishes `augur-macos.zip`
- Linux publishes `augur-linux.tar.gz`
- Windows publishes `augur-windows.zip`
- `release.toml` defines shared-version tagging as `v{{version}}` and the release commit/tag messaging
- `CHANGELOG.md` becomes the human-maintained ledger of user-facing release notes

Optional HDF5 replay remains excluded from tagged binaries because it depends on external native runtime components that are not bundled by the default release jobs.

## Consequences

### Positive

- Tagged releases now match the project's documented platform support
- macOS GUI downloads get a standard `.dmg` installer path instead of a raw `.app.zip`
- Release archives on every platform include the same supporting files and example config
- Maintainers have a repeatable shared-version release command instead of ad hoc version/tag edits

### Negative

- macOS GUI downloads remain unsigned, so Gatekeeper prompts are still expected
- The release workflow is broader than the previous single-job zip packaging
- CLI archives are still simple packaged binaries rather than native installers

## Alternatives Considered

### Keep macOS-only unsigned releases

Rejected because it conflicts with the documented multi-platform support story and keeps the macOS GUI distribution experience unnecessarily rough.
