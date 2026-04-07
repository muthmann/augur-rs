# Local Desktop Install

## Summary

AugurRS now documents a one-command local macOS install path for users who build from source:

```bash
./scripts/build-macos-app.sh --install
```

That command builds the release GUI binary, assembles `AugurRS.app`, and copies it into an
Applications directory so the app can be launched like a normal desktop application instead of via
`cargo run` on every session.

## User-Facing Behavior

- `scripts/build-macos-app.sh` builds `target/release/AugurRS`
- it assembles `dist/local-macos/AugurRS.app` using the same bundle path as the release workflow
- `--install` copies the app into `/Applications` by default
- `--install-dir <dir>` supports user-owned destinations such as `~/Applications`
- `--dmg` creates a local `AugurRS.dmg` for installer testing

## Platform Guidance

- **macOS**
  The recommended source-build path is now the helper script above. GitHub releases still provide
  an unsigned `AugurRS.dmg`, which may trigger Gatekeeper prompts on first launch.
- **Linux**
  GitHub releases are runnable archives after extraction, but they are not installer-managed
  desktop packages yet.
- **Windows**
  GitHub releases are runnable zip archives after extraction, but they are not installer-managed
  desktop packages yet.

## Files

| File | Role |
|---|---|
| `scripts/build-macos-app.sh` | one-command local macOS app bundle / install / DMG helper |
| `resources/macos-bundle.sh` | shared macOS bundle assembly used by releases and the local helper |
| `README.md` | quick-start guidance for local macOS app installation |
| `docs/getting-started.md` | recommended macOS source-install workflow |
| `docs/releases.md` | platform-specific expectations for current GitHub release artifacts |

## Verification

```bash
chmod +x scripts/build-macos-app.sh
./scripts/build-macos-app.sh --output-dir dist/local-macos-smoke
./scripts/build-macos-app.sh --output-dir dist/local-macos-smoke --dmg
./scripts/build-macos-app.sh --output-dir dist/local-macos-smoke --install --install-dir /tmp/augur-app-install
```
