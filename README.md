<div align="center">

<!-- TODO: Add project logo / banner here -->
<!-- ![AugurRS](resources/augur-banner.png) -->

# AugurRS

**A fast, direct event camera recorder and live preview tool for Prophesee EVK4 / IMX636 — written entirely in Rust.**

*No vendor runtime. No opaque SDK stack. Clean raw capture, GPU-accelerated live preview, full runtime sensor control, and a plugin system that lets you build any live analysis you need.*

[![CI](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml)
[![Release](https://github.com/muthmann/augur-rs/actions/workflows/release.yml/badge.svg)](https://github.com/muthmann/augur-rs/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![Language](https://img.shields.io/badge/language-Rust-orange)

</div>

---

AugurRS gives you direct, auditable control over an EVK4 event camera. Whether you are capturing raw event streams for computer vision research, running robotics experiments, doing high-speed measurements, or building a custom imaging workflow — the core tool does exactly what you need: reliable capture, live preview, recorded-file replay, and full sensor control. Nothing more, nothing less.

If you want to go further, the **plugin system** turns AugurRS into a live analysis surface. Plugins run alongside the preview stream and can do anything from signal processing to detection, tracking, metrics, and custom overlays. The plugin template, authoring docs, and community plugins live in the companion [**augur-plugins**](https://github.com/muthmann/augur-plugins) repository.

<!-- TODO: Add screenshot of augur-gui live preview here -->
<!-- ![Screenshot](resources/screenshot-gui.png) -->

---

## The Core Tool

AugurRS without any plugins is a complete, standalone event camera recorder and viewer.

### Capture

- **Direct hardware path** — Treuzell USB transport + IMX636 register programming, no Metavision or OpenEB required
- **Backpressured 3-thread pipeline** — USB reader → bounded disk writer → lossy preview decoder. Recording never blocks on the UI, never grows unbounded in memory
- **Reproducible sessions** — every `.raw` file gets a self-describing EVT3 header plus a `.toml` sidecar with config, provenance, and timing metadata
- **Replay in the GUI** — open recorded `.raw` captures or decoded `.csv`, `.bin`, `.npy`, and optional `.h5` / `.hdf5` event files, scrub them with a timeline, and run plugins through the same path used for live sessions
- **GPU-accelerated preview** — dual-backend renderer with wgpu (Metal, Vulkan, D3D12) and automatic OpenGL fallback. Shader-based colorization, gamma, and LUT mapping keep display-only changes off the CPU. Count-based preview modes can accumulate frames and build histograms on the GPU. Selectable via `AUGUR_RENDERER=auto|wgpu|glow`
- **Interactive preview workspace** — edge-collapsible side panels, pixel inspection, histogram-driven brightness/contrast, colormap switching, line/ruler/annotation tools, scale-bar overlay, enlarged popup preview, ImageJ streaming, and a toggleable 3D point-cloud view
- **Global settings menu** — top-bar control over pixel scale, sensor geometry, acquisition time, retained event history, and advanced preview/disk tuning
- **Live preview stats** — 1-second sliding window for Mev/s and MB/s, plus per-frame ON/OFF polarity percentages and per-stage renderer timings in the GUI

### Sensor Control

All of the following are adjustable live, mid-session:

| Control | Description |
|---|---|
| **Biases** | `diff_on`, `diff_off`, `fo`, `hpf`, `refr` — threshold and noise tradeoffs |
| **Hardware ROI** | Crop the active pixel area at the sensor level to reduce bandwidth |
| **DEM pixel mask** | 64-slot hardware mask to suppress noisy pixels before they hit the stream |
| **STC / Trail filters** | Mutually exclusive on-sensor noise filters |
| **Acquisition window** | Time window for event accumulation |

### Two Interfaces, One Backend

```bash
# CLI — for scripting and automation
augur status
augur record captures/run.raw --duration-s 30

# GUI — for live preview and interactive control
augur-gui
```

The CLI and GUI share the same config types. A TOML file saved from the GUI works directly with the CLI.

---

## Architecture

Six crates with explicit, enforced boundaries:

```
augur-cli ─────────────┐
                        ├──► augur-core
augur-gui ─────────────┼──► augur-prophesee
                        ├──► augur-plugin-api
runtime plugins ───────┼──► augur-plugin-types
                        └──► host-owned analysis UI in augur-gui
```

**`augur-core` is a pure camera SDK.** `augur-gui` owns the built-in analysis
tools (hotpixel detection, ROI-grid overlay) plus the runtime plugin host,
while plugin implementations live in the companion
[augur-plugins](https://github.com/muthmann/augur-plugins) repository.

### Streaming Pipeline

```
┌──────────────┐  raw EVT3       ┌──────────────┐  bounded   ┌──────────┐
│  USB reader   │ ─────────────► │  Disk writer  │ ────────► │ .raw file.│
└──────────────┘                 └──────────────┘            └──────────┘
        │
        │  lossy (drops frames, never blocks capture)
        ▼
┌──────────────┐  decoded frame  ┌──────────────────────────┐
│   Preview     │ ─────────────► │  Plugin host (optional)    │
│   decoder     │                 └──────────────────────────┘
└──────────────┘
```

The disk writer uses a **bounded channel** — recording pauses if the OS falls behind rather than growing unbounded. The preview decoder uses **lossy delivery** — frames are dropped rather than stalling the capture thread. These two properties together mean recording quality is independent of UI or analysis load.

---

## Quick Start

**Requirements:** Rust toolchain, Prophesee EVK4 over USB 3.

Tagged binary releases do not include HDF5 replay support. Use a source build if you need `.h5` / `.hdf5` replay.

```bash
# Build everything
cargo build --workspace

# Optional: enable `.h5` / `.hdf5` replay in the GUI
# Requires a system HDF5 installation such as `brew install hdf5`.
export HDF5_DIR="$(brew --prefix hdf5)"   # macOS / Homebrew
./scripts/install-ecf-plugin.sh
export HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin"
cargo build -p augur-gui --bin AugurRS --features hdf5

# Check camera connection
cargo run --bin augur -- status

# Record a 30-second capture
cargo run --bin augur -- record captures/run.raw --duration-s 30

# Launch the live GUI
cargo run -p augur-gui --bin AugurRS

# Launch the GUI with optional HDF5 replay support
HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin" cargo run -p augur-gui --bin AugurRS --features hdf5
```

On macOS, the easiest way to turn a source checkout into a normal app is:

```bash
./scripts/build-macos-app.sh --install
```

That builds `AugurRS.app` and copies it into `/Applications` so you can launch it from Finder,
Spotlight, or the Dock instead of rerunning `cargo run`. If `/Applications` needs admin
permissions, rerun with `sudo` or choose another destination such as
`--install-dir "$HOME/Applications"`. Add `--dmg` if you also want a local `AugurRS.dmg` for
testing.

For the full HDF5 / ECF setup, see [HDF5 File Support](./docs/features/hdf5-file-support.md).

Copy the example config for a local profile:

```bash
cp examples/augur.toml augur.toml
```

---

## Recording Output

Each capture produces two files:

```
captures/
  run.raw     ← EVT3 stream with geometry, device identity, software provenance, and pixel pitch
  run.toml    ← effective configuration plus recording metadata and timing for exact replay
```

---

## Plugin System

AugurRS ships a generic, FFI-based plugin system that turns the live preview into a programmable analysis surface. Plugins run frame-by-frame alongside the preview stream, declare what data they need (decoded frames, raw events, or upstream plugin results), and publish their output through a host-owned rendering pipeline — tables, scatter plots, density maps, line charts, image views.

Drop a compiled plugin into `~/.augur/plugins/`, and the GUI's **Plugin Manager** picks it up. Enable, disable, reload — no recompilation of the host required.

This repository ships the runtime plugin host and API crates. The plugin template, authoring docs, and plugin implementations are maintained in the companion [**augur-plugins**](https://github.com/muthmann/augur-plugins) repository. `augur-gui` itself includes built-in tools such as **Hotpixel Detection** as part of the core application rather than as plugins.

**Want to write a plugin?** See the [**Plugin Authoring Guide**](./docs/features/plugin-authoring-guide.md).

---

## Platform Support

| Platform | Status |
|---|---|
| macOS | Primary — hardware-tested |
| Linux | CI-verified on every push |
| Windows | CI-verified on every push |

Tagged releases ship macOS, Linux, and Windows CLI archives plus an unsigned macOS `AugurRS.dmg`. Optional HDF5 replay remains source-build-only.

- macOS: the GitHub DMG works, but Gatekeeper can prompt because it is unsigned. A local source
  build via `./scripts/build-macos-app.sh --install` is the smoothest path if you want a normal
  app in Applications without repeating terminal launch commands.
- Linux / Windows: the GitHub releases are runnable after extracting the archive, but they are
  still archive-style distributions rather than native installers.

---

## Documentation

| Document | Description |
|---|---|
| [Getting Started](./docs/getting-started.md) | Build, connect, first capture |
| [Configuration](./docs/configuration.md) | TOML reference: biases, ROI, mask, filters |
| [CLI Reference](./docs/cli.md) | Commands and scripting |
| [GUI Guide](./docs/gui.md) | Live preview, controls, the plugin panel, and the Plugin Manager |
| [Recording Format](./docs/recording.md) | EVT3 output and pipeline behavior |
| [Performance](./docs/performance.md) | Architecture and design rationale |
| [HDF5 File Support](./docs/features/hdf5-file-support.md) | Native HDF5 + ECF plugin setup for `.h5` / `.hdf5` replay |
| [Plugin Authoring Guide](./docs/features/plugin-authoring-guide.md) | Write your own plugin: FFI host, phases, context bus, host views |
| [Dynamic Plugin Loading](./docs/features/dynamic-plugins.md) | Plugin directory layout, manifests, scan/reload workflow |
| [GPU Preview Rendering](./docs/features/wgpu-preview-rendering.md) | Dual-backend wgpu/glow preview architecture and benchmark baseline |
| [Technical Notes](./docs/features/README.md) | SDK internals, built-in tools, and feature details |
| [Architecture Decisions](./docs/adr/README.md) | ADRs |

---

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

Plugin contributions go to the companion [augur-plugins](https://github.com/muthmann/augur-plugins) repository, which also hosts the plugin template and authoring docs.

---

## License

[MIT](./LICENSE)
