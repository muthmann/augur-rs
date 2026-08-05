# Release Distribution

## Summary

Every tagged AugurRS release publishes seven assets: a real installer and a plain archive per
platform, plus a checksum manifest.

| Asset | Platform | Role |
|---|---|---|
| `AugurRS-<version>-macos-universal.dmg` | macOS (arm64 + x86_64) | GUI installer **and** update payload |
| `augur-<version>-macos-universal.tar.gz` | macOS | CLI / terminal archive |
| `AugurRS-<version>-linux-x86_64.AppImage` | Linux | GUI install **and** update payload |
| `augur-<version>-linux-x86_64.tar.gz` | Linux | CLI / terminal archive |
| `AugurRS-<version>-windows-x86_64-setup.exe` | Windows | GUI installer **and** update payload |
| `augur-<version>-windows-x86_64.zip` | Windows | portable archive |
| `SHA256SUMS` | all | verification, and the digest source the in-app updater requires |

The single-file installer per platform is not a coincidence: it is what lets
[In-App Updates](./in-app-updates.md) apply an update without unpacking an archive. The filename
suffixes are a contract between `resources/packaging/` and `augur-update/src/target.rs`.

See [ADR 032](../adr/032-installer-distribution-and-in-app-updates.md) for the reasoning.

## Packaging Scripts

Packaging lives in `resources/packaging/`, not inline in the workflows, so any artifact can be
reproduced locally without pushing a commit:

```bash
bash resources/packaging/macos/build-dmg.sh            # -> dist/
bash resources/packaging/linux/build-appimage.sh       # -> dist/
pwsh resources/packaging/windows/build-installer.ps1   # -> dist\
```

Each accepts `--out DIR` (`-OutDir` on Windows) and `--skip-build` / `-SkipBuild` to reuse binaries
already in `target/release`. Version numbers come from `[workspace.package] version`, which
release-please bumps.

### macOS

Builds `aarch64-apple-darwin` and `x86_64-apple-darwin` and merges them with `lipo`, so one disk
image serves every Mac. `macos-latest` runners are Apple Silicon, so a single-target build had been
silently shipping an executable Intel Macs cannot run.

`resources/macos-bundle.sh` assembles `AugurRS.app` and derives `AugurRS.icns` from
`assets/logo.png`. The bundle is then ad-hoc signed. That is not notarization — it is required
because macOS refuses to launch a bundle whose signature no longer validates after in-place
modification, which is exactly what the updater does.

The DMG carries the app, an `Applications` symlink, and a first-launch note covering the
right-click-Open step Gatekeeper requires for unsigned downloads.

### Linux

`linuxdeploy` builds an AppImage carrying `AugurRS.desktop` and a 256×256 icon, so desktop
environments show a proper entry. `APPIMAGE_EXTRACT_AND_RUN=1` is set because GitHub runners have
no FUSE.

### Windows

`resources/packaging/windows/augur.nsi` builds an NSIS installer with a Start Menu shortcut, an
optional desktop shortcut, an icon, and an Add/Remove Programs entry with a working uninstaller.

It installs **per user**, into `%LOCALAPPDATA%\Programs\AugurRS`, because lab machines rarely grant
local admin and because the updater re-runs the installer with `/S` — a machine-wide install would
raise a UAC prompt that a silent run cannot answer. Machine-wide deployment is served by the
portable zip, which ships with a `AugurRS.cmd` launcher.

`assets/AugurRS.ico` is committed rather than generated, so no packaging host needs image tooling.

## How A Release Is Built

```
push to main
  └─ release-please.yml
       ├─ opens/updates the Release PR
       └─ on merge: creates the tag and release, then
            └─ calls release.yml (workflow_call, publish: true)
                 ├─ macOS / Linux / Windows build jobs
                 └─ publish job: SHA256SUMS + upload to the release
```

`release.yml` is **not** triggered by `on: push: tags`. release-please pushes the tag with
`GITHUB_TOKEN`, and GitHub does not start workflow runs from `GITHUB_TOKEN`-created events — which
is why `v1.0.0` shipped with zero assets. Calling the workflow directly, in the same run, avoids
this without needing a personal access token.

### Testing packaging without a tag

- `packaging.yml` runs the full matrix with `publish: false` on any pull request touching
  `resources/`, `assets/`, `.github/`, or the manifests. The installers are downloadable from the
  run's artifacts.
- `release.yml` also accepts `workflow_dispatch` with a `tag` and a `publish` toggle, for rebuilding
  an existing release or dry-running from a branch.

## Verifying A Download

```bash
sha256sum -c SHA256SUMS      # Linux
shasum -a 256 -c SHA256SUMS  # macOS
```

The in-app updater performs the same check automatically and treats a missing or mismatched digest
as fatal.

## Limits

- Tagged binaries exclude the optional `hdf5` feature, so `.h5` / `.hdf5` replay remains
  source-build-only. See [HDF5 File Support](./hdf5-file-support.md).
- macOS artifacts are ad-hoc signed, not notarized, so Gatekeeper prompts on first launch. The
  workflow is shaped so Developer ID signing can be added without restructuring it.
- Linux builds x86_64 only.
- The archives (`.tar.gz`, portable `.zip`) cannot self-update; only the installers can.
