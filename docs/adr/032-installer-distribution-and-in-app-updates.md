# ADR 032: Installer Distribution And In-App Updates

## Status

Accepted. Supersedes the distribution decisions in [ADR 004](./004-cross-platform-release-pipeline.md); the `cargo-release` and changelog conventions recorded there still stand.

## Context

ADR 004 set up a three-platform release pipeline. In practice it stopped producing anything:

- `v1.0.0` was published with **zero release assets**. release-please pushes the version tag using the default `GITHUB_TOKEN`, and GitHub does not start workflow runs from `GITHUB_TOKEN`-created events, so `release.yml`'s `on: push: tags` trigger never fired. There was no `workflow_dispatch` either, so the build could not be re-run by hand.
- CI only ran `cargo check` on Linux and Windows. The GUI was never linked on those platforms, and packaging was never exercised at all until a tag was pushed — the one moment when failure is least recoverable.
- `macos-latest` moved to Apple Silicon, so the artifact named `augur-macos.zip` had silently become arm64-only. Intel Macs received an executable they could not run.
- Linux and Windows shipped bare archives: no icon, no menu entry, no uninstall path.

Separately, lab machines drift. Users had no way to learn that a newer AugurRS existed, and updating meant finding the release page and repeating a manual install.

## Decision

### One update payload per platform

Each platform publishes exactly one file that is simultaneously the human download and the update payload:

| Platform | Artifact | Also published |
|---|---|---|
| macOS | `AugurRS-<version>-macos-universal.dmg` | `augur-<version>-macos-universal.tar.gz` |
| Linux | `AugurRS-<version>-linux-x86_64.AppImage` | `augur-<version>-linux-x86_64.tar.gz` |
| Windows | `AugurRS-<version>-windows-x86_64-setup.exe` | `augur-<version>-windows-x86_64.zip` |

Plus `SHA256SUMS` covering all of them.

This is the decision the rest follows from. Because the payload is always a single file, applying an update never unpacks an archive, and `augur-update` needs no zip, tar, or gzip dependency — only HTTP and SHA-256.

The suffixes are a contract between `resources/packaging/` and `augur-update/src/target.rs`. Renaming an artifact in one place without the other strands every installed copy, because an older build only knows the name it was compiled with.

### macOS builds universal, signed ad-hoc

Both `aarch64-apple-darwin` and `x86_64-apple-darwin` are built and `lipo`-merged, so one disk image serves every Mac. The bundle is ad-hoc signed (`codesign --sign -`), which is not a substitute for notarization but is required for a different reason: macOS refuses to launch a bundle whose signature no longer validates after in-place modification, which is exactly what the updater does.

Notarization is deferred because it needs a paid Apple Developer ID. Until then the DMG carries a first-launch note explaining the right-click-Open step, and the workflow is structured so signing secrets can be added later without reshaping it.

### Windows installs per user

The NSIS installer targets `%LOCALAPPDATA%\Programs\AugurRS` and runs as `RequestExecutionLevel user`. Two reasons, in order of weight:

1. Lab and shared machines rarely grant local admin to the person running the experiment. A per-user install works anyway.
2. The updater applies an update by re-running the installer with `/S`. A machine-wide install would raise a UAC prompt that a silent run cannot answer, so auto-update would fail with no visible cause.

Machine-wide deployment is served by the portable zip.

### Releases are triggered by a workflow call, not a tag

`release-please.yml` calls `release.yml` directly via `workflow_call`, in the same run, gated on `release_created`. This sidesteps the `GITHUB_TOKEN` restriction without introducing a personal access token. `release.yml` also accepts `workflow_dispatch` with a `publish` toggle, and `packaging.yml` calls it with `publish: false` on any pull request that touches packaging — so installers are built and downloadable long before a tag exists.

### The toolchain is pinned

`rust-toolchain.toml` pins the exact Rust version, read by CI through `actions-rust-lang/setup-rust-toolchain`. A clippy release must no longer be able to turn a green branch red without a code change. Bumping it is a deliberate commit with the lint fallout fixed in the same change.

### Updates are offered, never imposed

`augur-core` writes `augur_version` into every recording sidecar (`augur-core/src/metadata.rs`), so which binary produced a dataset is part of the scientific record. Three rules follow:

- **Downgrades are refused.** Only a strictly newer version is offered, so a developer build ahead of the last release is left alone.
- **Checksum verification is mandatory.** A release with no `SHA256SUMS`, an asset with no entry in it, or a digest mismatch aborts the update and discards the download. There is no "install anyway".
- **Installation is blocked while a recording or analysis run is active,** and never happens without an explicit click or `--yes`.

Whether this copy *can* be replaced is decided before downloading, not after: a source build, an extracted tarball, or a root-owned install reports why and links the release page instead.

## Consequences

### Positive

- Tagged releases publish binaries again, and the path is exercised on every packaging PR rather than only at tag time.
- Intel Macs are no longer shipped an arm64-only binary.
- Each platform gets a real install: Applications drag-and-drop, Start Menu plus Add/Remove Programs, or a single executable file with its own desktop entry.
- Users can update in place, and every download is verified against a published digest.
- Linux and Windows link failures now surface on the PR that causes them.

### Negative

- macOS downloads still trip Gatekeeper on first launch, because ad-hoc signing is not notarization.
- Linux ships x86_64 only; aarch64 Linux reports `UnsupportedPlatform` rather than guessing.
- The tar.gz and portable-zip archives cannot self-update — there is no single file to replace and nothing that owns the install location. They report this and link the release page.
- Windows machine-wide installs are not offered by the installer.
- CI is slower: three full builds instead of one build and two `cargo check`s. This is the cost of the failure mode it removes.

## Alternatives Considered

**`cargo-dist`.** Generates the release workflow and installers, and pairs with `axoupdater` for self-update. Rejected because it has no macOS `.app` bundle support, and the bundle is the primary macOS deliverable here. Adopting it would have meant keeping a hand-written macOS path anyway, for less control over the rest.

**The `self_update` crate.** Well-tested, but built around replacing a single executable, which is not how a macOS `.app` bundle or an NSIS install is updated. It also pulls in `reqwest` and therefore a tokio runtime the app does not otherwise need. The bundle-swap and installer-relaunch logic would have had to be written regardless.

**A personal access token for the tag trigger.** Would have made `on: push: tags` work. Rejected because it adds a credential to rotate and a failure mode that is invisible when it expires; calling the workflow directly needs no secret at all.

**MSI via WiX instead of NSIS.** Better for group-policy deployment, but the MSI database fights in-place updates: every update would have to re-run the full installer through Windows Installer, and per-user MSI installs are awkward. NSIS with `/S` is what makes the update path simple.

**`.deb` packages for Linux.** Would give a native install on Debian and Ubuntu, but installs into root-owned paths, which blocks in-app updates, and covers only part of the distro landscape. An AppImage runs everywhere and is trivially replaceable.

**Silent background updates.** Rejected outright. Recording provenance makes the running version part of the data.
