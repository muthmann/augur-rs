# AugurRS

> Rust-first tooling for Prophesee EVK4 / IMX636 event cameras. Fast CLI capture, native desktop GUI, and a small codebase you can actually audit.

[![CI](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml)
[![Release](https://github.com/muthmann/augur-rs/actions/workflows/release.yml/badge.svg)](https://github.com/muthmann/augur-rs/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

<!-- screenshot: desktop GUI preview placeholder -->
<!-- screenshot: ROI-grid and hotpixel workflow placeholder -->

## Highlights

- **Built around the real hardware**: direct Treuzell USB transport and IMX636 register programming, no vendor runtime required
- **Small, explicit architecture**: four crates, one bounded streaming pipeline, no giant framework stack
- **Two interfaces, one backend**: `augur` for scripting and capture automation, `augur-gui` for live preview and runtime tuning
- **Preview that stays out of the way**: EVT3 decoding via [`evt3-core`](https://crates.io/crates/evt3-core/0.1.0) on a lossy path so capture stays prioritized
- **Runtime tuning and analysis**: biases, ROI, DEM mask, STC/Trail filters, hotpixel detection, and ROI-grid assistance
- **Reproducible sessions**: every `.raw` recording writes the effective TOML sidecar next to the capture

## Built for macOS

AugurRS is designed and hardware-validated on macOS first. The workspace remains portable Rust (`rusb` + `eframe`), CI checks Linux and Windows compilation, and tagged releases now package both a CLI archive and an unsigned macOS `.app` bundle for the GUI.

## Architecture

```text
                 +---------------------------+
                 |         augur-cli         |
                 |      `augur` binary       |
                 +-------------+-------------+
                               |
+-------------------+          |          +-------------------+
| augur-prophesee   +----------+----------+   augur-gui       |
| EVK4 / IMX636 I/O |                     | `augur-gui` app   |
+---------+---------+                     +---------+---------+
          \                                         /
           \                                       /
            +----------------+--------------------+
                             |
                     +-------v-------+
                     |  augur-core   |
                     | config, pipe, |
                     | preview, anal |
                     +---------------+
```

The libraries stay small and reusable. The binaries stay thin. That keeps hardware behavior visible and makes it practical to debug throughput, recording, and runtime-control issues without reverse-engineering a large vendor stack.

## Quickstart

**Requirements:** macOS, Rust toolchain, and a direct USB 3 connection to a Prophesee EVK4 with IMX636.

```bash
# Build the workspace
cargo build --workspace

# Probe the camera
cargo run --bin augur -- status

# Record a 10-second capture
cargo run --bin augur -- record captures/session.raw --duration-s 10

# Launch the desktop app
cargo run --bin augur-gui
```

AugurRS uses `augur.toml` by default for mutable CLI configuration. Start from the example config if you want a local profile:

```bash
cp examples/augur.toml augur.toml
cargo run --bin augur -- config show --config augur.toml
```

## AugurRS vs. Prophesee Stack

| | **AugurRS** | **Prophesee Metavision / OpenEB** |
|---|---|---|
| Language | Rust | C++ / Python |
| Scope | EVK4 + IMX636 focused | Broad multi-sensor ecosystem |
| Dependencies | Minimal (`rusb`, `eframe`) | Large SDK with plugin system |
| Interface | CLI + native GUI | APIs, samples, Studio |
| Release form | Source builds, release zips, macOS `.app` | Vendor installers and SDK packages |
| Platform posture | macOS first, CI checks Linux/Windows builds | Linux and Windows officially supported |
| Codebase shape | Small workspace, direct control path | Large multi-module stack |

Choose AugurRS when you want a compact, Rust-native path to EVK4 capture and control. Choose the official stack when you need the broader vendor ecosystem, algorithms, or officially packaged multi-platform tooling.

## Documentation

- [Getting started](./docs/getting-started.md)
- [Configuration reference](./docs/configuration.md)
- [CLI usage](./docs/cli.md)
- [GUI usage](./docs/gui.md)
- [Recording format](./docs/recording.md)
- [Performance notes](./docs/performance.md)
- [Release notes](./docs/releases.md)
- [Technical notes](./docs/features/README.md)
- [Architecture decisions](./docs/adr/README.md)
- [mdBook site](https://uthmann.github.io/augur-rs/)

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

## License

[MIT](./LICENSE)
