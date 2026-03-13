# Releases

## Distribution

AugurRS ships in five forms:

| Channel | Audience | Contents |
|---------|----------|----------|
| **Source** | Developers | `cargo build --workspace` from the repository |
| **macOS CLI archive** | Terminal users | `augur-macos.zip` — prebuilt `augur` / `augur-gui` binaries, docs, example config, changelog |
| **macOS GUI installer** | Desktop users | `AugurGUI.dmg` — unsigned drag-to-Applications installer |
| **Linux archive** | Terminal and desktop users | `augur-linux.tar.gz` — prebuilt Linux binaries, docs, example config, changelog |
| **Windows archive** | Terminal and desktop users | `augur-windows.zip` — prebuilt Windows binaries, docs, example config, changelog |

All release artifacts are built by GitHub Actions and attached to every tagged release.

## Release Workflow

Before creating a release, update [`CHANGELOG.md`](../CHANGELOG.md) with the user-facing changes for the upcoming version.

Use `cargo-release` from the repository root to bump the shared workspace version, commit the version change, create the `v<version>` tag, and push it:

```bash
cargo install cargo-release
cargo release patch --dry-run
cargo release patch --execute
```

On a pushed version tag, the CI pipeline:

1. Builds `augur` and `augur-gui` on macOS, Linux, and Windows
2. Packages the macOS CLI archive, Linux tarball, and Windows zip with binaries, docs, example config, and changelog
3. Assembles `AugurGUI.app`, stages it alongside an `Applications` symlink, and wraps it in `AugurGUI.dmg`
4. Uploads all four release artifacts to the GitHub Release

## Known Limitations

- Tagged binaries still build without the optional `hdf5` feature, so `.h5` / `.hdf5` replay remains a source-build-only workflow.
- The macOS `.dmg` is still unsigned, so Gatekeeper may require the usual right-click → Open workaround on first launch.
- The raw CLI archives are packaged convenience artifacts rather than installer-managed distributions.
- Windows releases assume the default workspace feature set. If optional native features are enabled in the future, the release job may need extra dependency packaging.
