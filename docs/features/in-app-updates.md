# In-App Updates

## Summary

AugurRS can check for a newer release and install it without leaving the app. The GUI exposes this
under `Help ▸ Check for updates…`, and the CLI as `augur update`. Both are backed by the
`augur-update` crate.

Updates are **offered, never imposed**. `augur-core` writes `augur_version` into every recording
sidecar, so which binary produced a dataset is part of the scientific record — an update that
happened silently mid-session would leave that record describing a build that never wrote the file.

See [ADR 032](../adr/032-installer-distribution-and-in-app-updates.md) for the decisions behind
this, and [Release Distribution](./release-distribution.md) for the artifacts it consumes.

## User-Facing Behaviour

### GUI

- `Help ▸ Check for updates…` runs a check immediately and opens the update window.
- `Help ▸ Check on startup` (on by default) runs the same check in the background at most once
  every 24 hours. It never blocks startup and never blocks a frame.
- A newer release raises a toast and opens a window with the version, the release notes, and a
  **Download and install** button. **Skip this version** silences the automatic prompt for that
  version only; an explicit check still shows it.
- Progress is shown while downloading. When the update is applied, AugurRS restarts.
- A failed background check is silent — a lab machine is often simply offline. A check the user
  asked for always reports what went wrong.

### CLI

```bash
augur update --check-only   # report what is available, change nothing
augur update                # check, confirm, download, verify, install
augur update --yes          # same, without the confirmation prompt
```

### Turning it off

- `AUGUR_NO_UPDATE_CHECK=1` disables checking at runtime, for managed or air-gapped deployments.
- Building without the default `self-update` feature compiles the updater out entirely:

  ```bash
  cargo build -p augur-gui --bin AugurRS --no-default-features
  ```

- `AUGUR_UPDATE_REPO=owner/name` points the updater at a different repository, for testing a
  release before it is public.

## What It Will Not Do

| Rule | Behaviour |
|---|---|
| No downgrades | Only a strictly newer version is offered. A build ahead of the last release reports "up to date" rather than reinstalling backwards over itself. |
| No unverified payload | A release without `SHA256SUMS`, an asset with no entry in it, or a digest mismatch aborts and deletes the download. There is no "install anyway". |
| No update mid-acquisition | Blocked while a recording or an analysis run is active, with the reason shown. |
| No writing where it does not belong | A source build, an extracted tarball, or an install location this user cannot write to reports why and links the release page. Nothing is downloaded first. |
| No prereleases | Drafts and prereleases are skipped, even if the feed offers one. |

## How An Update Is Applied

Each platform publishes one file that is both the human download and the update payload, which is
why applying an update never involves unpacking an archive.

| Platform | Payload | Mechanism |
|---|---|---|
| macOS | `.dmg` | `hdiutil attach` → `ditto` the new bundle beside the installed one → rename the old aside, rename the new in → re-apply the ad-hoc signature → relaunch. A failure part-way leaves a complete bundle at one path or the other, never a half-overwritten one. |
| Windows | `-setup.exe` | Spawn the installer with `/S` and exit. The installer stops the running `AugurRS.exe` and replaces the files. Per-user install means no UAC prompt a silent run could not answer. |
| Linux | `.AppImage` | Copy beside the running image, `chmod 755`, then `rename` over it. The rename is atomic within one filesystem and sidesteps `ETXTBSY`: the running process keeps the old inode while the directory entry moves on. Uses `$APPIMAGE`, not `current_exe()`, which points into a mount that disappears on exit. |

## Where State Lives

Update preferences live in a small JSON file in the platform config directory
(`~/.config/augur/updates.json` on Linux, `~/Library/Application Support/augur/updates.json` on
macOS, `%APPDATA%\augur\updates.json` on Windows):

```json
{
  "check_on_startup": true,
  "last_check_unix": 1785312000,
  "skipped": null
}
```

Deliberately **not** part of `CameraConfig`. That struct is serialised into recording sidecars, and
how often a machine checks for updates is not experiment metadata. A corrupt or unreadable prefs
file falls back to defaults rather than blocking startup.

## Crate Layout

`augur-update` is its own workspace crate so `augur-core` stays a pure camera SDK and the GUI and
CLI share one implementation. Its only non-workspace dependency is `ureq` (blocking HTTP with
rustls) — the app has no async runtime and should not grow one to fetch a JSON document and a file.

| Module | Responsibility |
|---|---|
| `version.rs` | Small semver type: parse, order, refuse malformed input. Release tags are plain `vMAJOR.MINOR.PATCH`, so this is the whole grammar needed. |
| `target.rs` | Which asset suffix this build installs, and what kind of payload it is. |
| `feed.rs` | GitHub releases API, asset selection, and the reasons a release is unusable. |
| `checksum.rs` | `SHA256SUMS` parsing and streaming file digests. |
| `install.rs` | Per-platform apply, and the pre-flight check for whether this copy can be replaced at all. |

Public surface: `check`, `download`, `apply`, `discard`, `install_kind`, `releases_url`.

## Limits

- Linux is x86_64 only; other architectures report `UnsupportedPlatform`.
- The `.tar.gz` and portable `.zip` archives cannot self-update — there is no single file to
  replace and nothing that owns the install location.
- macOS downloads are ad-hoc signed rather than notarized, so Gatekeeper still prompts on the first
  launch of a manually downloaded copy. An update applied in place is unaffected, because the
  replacement inherits the trust already granted to the installed bundle.
- The check reads the public GitHub API unauthenticated, which is rate-limited per IP. The 24-hour
  throttle keeps normal use far below that.
