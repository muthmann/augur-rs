# Releases

## Distribution

Every tagged release publishes an installer and a plain archive per platform, plus `SHA256SUMS`.

| Channel | Audience | Asset |
|---------|----------|-------|
| **macOS installer** | Desktop users | `AugurRS-<version>-macos-universal.dmg` — drag to Applications, runs on Apple Silicon and Intel |
| **macOS archive** | Terminal users | `augur-<version>-macos-universal.tar.gz` — `augur` / `AugurRS` binaries, docs, example config |
| **Linux installer** | Desktop users | `AugurRS-<version>-linux-x86_64.AppImage` — one executable file, no install step |
| **Linux archive** | Terminal and HPC users | `augur-<version>-linux-x86_64.tar.gz` |
| **Windows installer** | Desktop users | `AugurRS-<version>-windows-x86_64-setup.exe` — Start Menu entry, icon, uninstaller |
| **Windows archive** | Portable use | `augur-<version>-windows-x86_64.zip` — includes a `AugurRS.cmd` launcher |
| **Source** | Developers | `cargo build --workspace` |

All artifacts are built in public by GitHub Actions. Verify any download against the published
digests:

```bash
sha256sum -c SHA256SUMS      # Linux
shasum -a 256 -c SHA256SUMS  # macOS
```

## Installing

**macOS** — open the DMG, drag `AugurRS.app` to Applications. On first launch, right-click the app
and choose **Open** (a plain double-click is refused). The build is ad-hoc signed rather than
notarized, so Gatekeeper asks once. If macOS claims the app is damaged, clear the quarantine flag:

```bash
xattr -dr com.apple.quarantine /Applications/AugurRS.app
```

**Linux** — `chmod +x AugurRS-*.AppImage` and run it. No install step, no root, no distro packages.

**Windows** — run the setup. It installs per user into `%LOCALAPPDATA%\Programs\AugurRS`, so no
admin rights are needed, and it appears in Add/Remove Programs like any other application.

## Staying Up To Date

AugurRS updates itself. `Help ▸ Check for updates…` in the GUI, or `augur update` on the command
line; a background check runs at most once a day and can be switched off. Downloads are verified
against `SHA256SUMS` before anything is replaced, and an update is refused while a recording or
analysis run is active. See [In-App Updates](./features/in-app-updates.md).

The `.tar.gz` and portable `.zip` archives cannot self-update — install from the DMG, AppImage, or
setup if you want that.

## Release Workflow

Releases are automated via [release-please](https://github.com/googleapis/release-please). No manual
tagging is needed.

1. Merge PRs to `main` using [conventional commits](https://www.conventionalcommits.org/):
   - `feat: …` → minor bump
   - `fix: …` → patch bump
   - `feat!: …` or a `BREAKING CHANGE:` footer → major bump
   - `chore:`, `docs:`, `test:` → no bump
2. release-please opens or updates a **Release PR** accumulating `CHANGELOG.md` entries and the
   `Cargo.toml` version bump.
3. Merging the Release PR creates the tag and GitHub release, and — in the same workflow run —
   calls the release build, which attaches all seven assets.

### Why the build is not triggered by the tag

release-please pushes the tag using the default `GITHUB_TOKEN`, and GitHub deliberately does not
start workflow runs from `GITHUB_TOKEN`-created events. A `on: push: tags` trigger therefore never
fires — which is why `v1.0.0` was published with zero assets. `release-please.yml` calls
`release.yml` directly instead, which needs no personal access token.

### Testing packaging without cutting a tag

- Any pull request touching `resources/`, `assets/`, `.github/`, or the manifests runs the
  **Packaging** workflow, which builds every installer and publishes nothing. Download them from the
  run's artifacts.
- **Release** can also be started manually (`workflow_dispatch`) with a `tag` and a `publish`
  toggle, to rebuild an existing release or dry-run from a branch.

### Manual release (fallback)

```bash
cargo install cargo-release
cargo release patch --dry-run
cargo release patch --execute
```

A tag pushed this way carries a real user's credentials, so it does trigger workflows normally.

## Toolchain

`rust-toolchain.toml` pins the exact Rust version, and CI reads it rather than tracking `stable`.
An unpinned toolchain meant a new clippy release could turn a green branch red without a code
change. Bump it deliberately, in its own commit, with the lint fallout fixed.

Use the rustup shim (`~/.cargo/bin/cargo`) locally — a Homebrew-installed `cargo` earlier on `PATH`
ignores `rust-toolchain.toml`, so checks would run against a different compiler than CI.

## Known Limitations

- Tagged binaries build without the optional `hdf5` feature, so `.h5` / `.hdf5` replay stays
  source-build-only.
- macOS artifacts are ad-hoc signed, not notarized, so Gatekeeper prompts on first launch.
- Linux builds x86_64 only.
- The Windows installer is per-user by design; machine-wide deployment uses the portable zip.
