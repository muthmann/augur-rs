# AugurRS

**A Rust toolkit for Prophesee EVK4 / IMX636 event cameras.**
Direct hardware control, zero vendor runtime, fast CLI capture, and a native desktop GUI — in under 10k lines of Rust.

[![CI](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml)
[![Release](https://github.com/muthmann/augur-rs/actions/workflows/release.yml/badge.svg)](https://github.com/muthmann/augur-rs/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)

<!-- screenshot: GUI preview -->

## Why AugurRS

Prophesee's official stack (Metavision / OpenEB) is a large C++/Python ecosystem built for broad sensor support. AugurRS takes a different approach: a focused Rust workspace that talks directly to the EVK4 hardware over USB, with no SDK layers in between.

- **Direct hardware path** — Treuzell USB transport and IMX636 register programming without vendor libraries
- **Bounded streaming pipeline** — three-thread architecture: USB reader, backpressured disk writer, lossy preview decoder. Capture never stalls for the UI
- **Two interfaces, one backend** — `augur` CLI for scripting and automation, `augur-gui` for live preview and runtime tuning
- **Runtime sensor control** — biases, hardware ROI, 64-slot pixel mask (DEM), STC/Trail filters, acquisition window — all adjustable mid-session
- **Hotpixel workflow** — detect, mask, and compute optimal ROI regions around defective pixels
- **Reproducible captures** — every `.raw` recording writes the effective TOML config as a sidecar

## Architecture

```
augur-cli ─────────┐
                    ├──→ augur-core (config, pipeline, preview, analysis)
augur-gui ─────────┤
                    └──→ augur-prophesee (EVK4 transport, IMX636 registers)
```

Four crates, clear boundaries. The libraries are reusable, the binaries are thin wrappers, and the full hardware control path is auditable.

## Quick Start

**Requirements:** macOS (primary), Rust toolchain, Prophesee EVK4 over USB 3.

```bash
cargo build --workspace

# Probe the camera
cargo run --bin augur -- status

# Record a 10-second capture
cargo run --bin augur -- record captures/session.raw --duration-s 10

# Launch the GUI
cargo run --bin augur-gui
```

Copy the example config for a local profile:

```bash
cp examples/augur.toml augur.toml
```

## Platform Support

AugurRS is developed and hardware-tested on macOS. The codebase is portable Rust (`rusb` + `eframe`) — CI verifies Linux and Windows builds on every push. Tagged releases ship a CLI archive and an unsigned macOS `.app` bundle.

## Documentation

- [Getting Started](./docs/getting-started.md) — build, connect, first capture
- [Configuration](./docs/configuration.md) — TOML reference for biases, ROI, mask, filters
- [CLI Usage](./docs/cli.md) — commands and scripting
- [GUI Usage](./docs/gui.md) — live preview, controls, analysis tools
- [Recording Format](./docs/recording.md) — EVT3 output and pipeline behavior
- [Performance](./docs/performance.md) — architecture and design rationale
- [Releases](./docs/releases.md) — distribution and packaging
- [Technical Notes](./docs/features/README.md) — SDK internals and ROI-grid algorithm
- [Architecture Decisions](./docs/adr/README.md) — ADRs

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

## License

[MIT](./LICENSE)
