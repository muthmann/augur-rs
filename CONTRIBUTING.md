# Contributing

Contributions are welcome, but changes should stay tight, testable, and documented.

## Before You Start

- Open an issue or start a discussion before large refactors or major feature additions
- Keep hardware scope explicit when a change is specific to EVK4 or IMX636 behavior
- Prefer small pull requests over broad mixed-purpose changes

## Toolchain

The Rust version is pinned in `rust-toolchain.toml`, and CI reads it from there. Use the rustup
shim so your checks run against the same compiler:

```bash
cargo --version   # must match rust-toolchain.toml
```

If it does not, another `cargo` is earlier on your `PATH` — a Homebrew-installed one, for instance,
which ignores `rust-toolchain.toml` entirely. Put `~/.cargo/bin` first, or call
`~/.cargo/bin/cargo` explicitly. Checks that pass against a different compiler prove nothing about
CI.

## Development Checklist

Run the relevant checks before opening a pull request:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Changes that touch packaging can be verified without pushing a tag:

```bash
bash resources/packaging/macos/build-dmg.sh            # -> dist/
bash resources/packaging/linux/build-appimage.sh       # -> dist/
pwsh resources/packaging/windows/build-installer.ps1   # -> dist\
```

CI builds all of them on any pull request that touches `resources/`, `assets/`, `.github/`, or the
manifests.

If your change touches hardware-facing behavior, include the result of any manual EVK4 validation you performed.

## Documentation Expectations

Update documentation when behavior changes:

- `README.md` for front-door usage changes
- files in `docs/` for setup, configuration, CLI, GUI, or recording changes
- `docs/features/` for deeper technical behavior notes
- `docs/adr/` for long-lived architectural decisions

## Pull Request Notes

Include:

- what changed
- how you verified it
- any hardware, platform, or sensor assumptions
