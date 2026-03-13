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

Releases are automated via [release-please](https://github.com/googleapis/release-please). No manual tagging is needed.

### Day-to-day flow

1. Merge PRs to `main` using [conventional commits](https://www.conventionalcommits.org/):
   - `feat: …` — new feature → minor version bump
   - `fix: …` — bug fix → patch version bump
   - `feat!: …` or `BREAKING CHANGE:` footer → major version bump
   - `chore:`, `docs:`, `test:` — no version bump
2. After each merge, release-please opens or updates a **"Release PR"** that accumulates `CHANGELOG.md` entries and bumps `Cargo.toml`.
3. When the team is ready to ship, merge the Release PR.
4. release-please pushes a `v<version>` tag → the CI release pipeline triggers automatically.

### What the CI release pipeline does

On a pushed version tag, the pipeline:

1. Builds `augur` and `augur-gui` on macOS, Linux, and Windows
2. Packages the macOS CLI archive, Linux tarball, and Windows zip with binaries, docs, example config, and changelog
3. Assembles `AugurGUI.app`, stages it alongside an `Applications` symlink, and wraps it in `AugurGUI.dmg`
4. Uploads all four release artifacts to the GitHub Release

### Manual release (fallback)

If you need to release without release-please, use `cargo-release`:

```bash
cargo install cargo-release
cargo release patch --dry-run   # preview changes
cargo release patch --execute   # bump, tag, push → triggers CI
```

### Conventional commit → version bump reference

| Commit prefix | Version bump |
|---|---|
| `fix:` | patch (0.1.0 → 0.1.1) |
| `feat:` | minor (0.1.0 → 0.2.0) |
| `feat!:` or `BREAKING CHANGE:` | major (0.1.0 → 1.0.0) |
| `chore:`, `docs:`, `test:` | no bump |

## Known Limitations

- Tagged binaries still build without the optional `hdf5` feature, so `.h5` / `.hdf5` replay remains a source-build-only workflow.
- The macOS `.dmg` is still unsigned, so Gatekeeper may require the usual right-click → Open workaround on first launch.
- The raw CLI archives are packaged convenience artifacts rather than installer-managed distributions.
- Windows releases assume the default workspace feature set. If optional native features are enabled in the future, the release job may need extra dependency packaging.
