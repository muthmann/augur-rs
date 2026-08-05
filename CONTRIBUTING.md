# Contributing

Contributions are welcome, but changes should stay tight, testable, and documented.

## Before You Start

- Open an issue or start a discussion before large refactors or major feature additions
- Keep hardware scope explicit when a change is specific to EVK4 or IMX636 behavior
- Prefer small pull requests over broad mixed-purpose changes

## Development Checklist

Run the relevant checks before opening a pull request:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

With [`just`](https://github.com/casey/just) installed (`cargo install just`), `just ci` runs the
same steps in the same order as the GitHub Actions pipeline.

CI tests every pull request on macOS, Linux, and Windows, and on both arm64 and x86_64. Threading
bugs and FFI signedness mismatches often surface on only one architecture, so a green run on your
own machine is not sufficient — wait for the full matrix.

If your change touches hardware-facing behavior, include the result of any manual EVK4 validation you performed.

## Platform Setup

### Windows

Building requires the Visual Studio Build Tools with the C++ workload **for your machine's
architecture**. On an arm64 device (Snapdragon, Windows on ARM) the x64 workload alone is not
enough: build scripts and proc macros are always compiled for the host, so linking fails with
`LNK2001: unresolved external symbol memcpy` even when cross-compiling. Install the arm64
component from an elevated shell:

```
& "C:\Program Files (x86)\Microsoft Visual Studio\Installer\setup.exe" modify `
  --installPath "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools" `
  --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 --passive --norestart
```

Verify that `VC\Tools\MSVC\<version>\lib` contains an `arm64` directory afterwards.

Also note that Git for Windows ships an unrelated GNU `link.exe` in its `usr/bin`. If it precedes
the MSVC linker in `PATH`, cargo fails with `link: extra operand ... Try 'link --help'`. Build from
PowerShell or a Developer Command Prompt rather than Git Bash, and check with `where.exe link.exe`
if you hit that error.

### Linux

```bash
sudo apt-get install -y libxkbcommon-dev libx11-dev libxi-dev \
  libgl1-mesa-dev libegl1-mesa-dev pkg-config
```

Add `libhdf5-dev` if you build with the optional `hdf5` feature.

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
