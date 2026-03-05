# Releases

## Distribution

AugurRS ships in three forms:

| Channel | Audience | Contents |
|---------|----------|----------|
| **Source** | Developers | `cargo build --workspace` from the repository |
| **CLI archive** | Terminal users | `augur-macos.zip` — prebuilt binaries, docs, example config |
| **macOS app** | Desktop users | `AugurGUI.app.zip` — unsigned `.app` bundle for the GUI |

Both archives are built by GitHub Actions and attached to every tagged release.

## Release Workflow

On a version tag push, the CI pipeline:

1. Builds `augur` and `augur-gui` for macOS
2. Packages the CLI archive with binaries, documentation, and example config
3. Assembles `AugurGUI.app` from `resources/Info.plist` and the GUI binary
4. Uploads both `.zip` archives to the GitHub Release

## Known Limitations

- The `.app` bundle is **unsigned** — macOS Gatekeeper will prompt on first launch. Right-click → Open to bypass.
- No notarization or installer-based distribution yet.
- Code signing is the next step if broader GUI distribution becomes a priority.
