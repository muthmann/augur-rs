<div align="center">

# AugurRS

**A fast, direct event camera recorder and live preview tool for Prophesee EVK4 / IMX636 — written entirely in Rust.**

*No vendor runtime. No opaque SDK stack. Clean raw capture, full runtime sensor control, and a plugin system that can run any live analysis you want.*

[![CI](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/muthmann/augur-rs/actions/workflows/ci.yml)
[![Release](https://github.com/muthmann/augur-rs/actions/workflows/release.yml/badge.svg)](https://github.com/muthmann/augur-rs/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
![Platform](https://img.shields.io/badge/platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)
![Language](https://img.shields.io/badge/language-Rust-orange)

</div>

---

AugurRS gives you direct, auditable control over an EVK4 event camera. Whether you are capturing raw event streams for computer vision research, running robotics experiments, doing high-speed measurements, or building a custom imaging workflow — the core tool does exactly what you need: reliable capture, live preview, recorded-file replay, and full sensor control. Nothing more, nothing less.

If you want to go further, the **plugin system** turns AugurRS into a live analysis surface. Plugins run alongside the preview stream and can do anything — signal processing, detection, localization, metrics, custom overlays. Scientific plugins for SMLM microscopy and biophotonics workflows are maintained in the companion [**augur-plugins**](#plugin-ecosystem) repository.

---

## The Core Tool

AugurRS without any plugins is a complete, standalone event camera recorder and viewer.

### Capture

- **Direct hardware path** — Treuzell USB transport + IMX636 register programming, no Metavision or OpenEB required
- **Backpressured 3-thread pipeline** — USB reader → bounded disk writer → lossy preview decoder. Recording never blocks on the UI, never grows unbounded in memory
- **Reproducible sessions** — every `.raw` file gets a `.toml` config sidecar written automatically
- **Replay in the GUI** — open recorded `.raw` captures instantly, scrub them with a timeline, and run plugins through the same path used for live sessions
- **Throughput stats** — 1-second sliding window for Mev/s and MB/s, visible in the GUI and CLI

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

Four crates with explicit, enforced boundaries:

```
augur-cli ─────────────┐
                        ├──► augur-core
augur-gui ─────────────┤      camera traits · TOML config · streaming pipeline
         │              │      EVT3 preview · analysis plugin API
         │              └──► augur-prophesee
         │                    EVK4 Treuzell transport · IMX636 ISSD registers
         └──► plugin host  ← plugins live here, not in augur-core
```

**`augur-core` is a pure camera SDK.** The plugin host in `augur-gui` is the only place your data analysation pipeline can live.

### Streaming Pipeline

```
┌──────────────┐  raw EVT3       ┌──────────────┐  bounded   ┌──────────┐
│  USB reader  │ ──────────────► │  Disk writer │ ─────────► │ .raw file│
└──────────────┘                 └──────────────┘            └──────────┘
        │
        │  lossy (drops frames, never blocks capture)
        ▼
┌──────────────┐  decoded frame  ┌──────────────────────────┐
│   Preview    │ ──────────────► │  Plugin host (optional)  │
│   decoder    │                 └──────────────────────────┘
└──────────────┘
```

The disk writer uses a **bounded channel** — recording pauses if the OS falls behind rather than growing unbounded. The preview decoder uses **lossy delivery** — frames are dropped rather than stalling the capture thread. These two properties together mean recording quality is independent of UI or analysis load.

---

## Quick Start

**Requirements:** Rust toolchain, Prophesee EVK4 over USB 3.

```bash
# Build everything
cargo build --workspace

# Check camera connection
cargo run --bin augur -- status

# Record a 30-second capture
cargo run --bin augur -- record captures/run.raw --duration-s 30

# Launch the live GUI
cargo run --bin augur-gui
```

Copy the example config for a local profile:

```bash
cp examples/augur.toml augur.toml
```

---

## Recording Output

Each capture produces two files:

```
captures/
  run.raw     ← EVT3 stream  (header: format EVT3;width=1280;height=720)
  run.toml    ← effective configuration snapshot for exact replay
```

---

## Plugin Ecosystem

> **Plugins live in a separate repository.**
> The core AugurRS repo defines the plugin API and hosts the plugin runtime. Plugin implementations — including the scientific analysis suite — are maintained at **[augur-plugins](https://github.com/muthmann/augur-plugins)** *(coming soon)*.

The plugin system is a first-class extension point, not an afterthought. Plugins:

- run live alongside the preview stream, frame by frame
- share typed derived data with each other through a per-frame context bus
- declare their input kind (decoded frame, raw events, or upstream plugin results)
- run in ordered phases so one plugin can feed the next within the same frame
- are compiled in — adding or removing a plugin is a one-line registration change

Anyone can write a plugin. The API is documented in [Plugin Architecture](./docs/features/analysis-plugins.md).

### What Plugins Can Do

Plugins can operate on anything the preview pipeline produces:

| Input kind | Example use cases |
|---|---|
| Decoded preview frame | Overlay rendering, pixel statistics, motion detection, sharpness estimation |
| Raw `CdEvent` stream | Event-based reconstruction, spatiotemporal filtering, spike detection |
| Upstream plugin results | Chained metrics, decision logic that consumes another plugin's output |

### Available Plugins

The **[augur-plugins](https://github.com/muthmann/augur-plugins)** repository is the central home for plugin implementations. It currently includes:

| Plugin | What it does |
|---|---|
| **Hotpixel Detection** | Detects persistently noisy pixels live and pushes them into the hardware DEM mask |
| **ROI Grid** | Partitions the sensor around masked hotpixels; finds the largest clean capture regions with one-click "Use as ROI" |
| **Molecule Localization** | Wavelet denoising, center-of-mass seeding, sub-pixel elliptical Gaussian fitting — SMLM-grade localization from the live event stream |
| **Focus Metrics** | Mean PSF sigma, FFT high-frequency power, and astigmatic ratio — live focus feedback for event-camera imaging |

These plugins are purpose-built for SMLM microscopy and biophotonics, but they also illustrate the full range of what the plugin API supports. The same system can host any domain-specific live analysis.

---

## Why Not Metavision / OpenEB?

| | OpenEB | AugurRS |
|---|---|---|
| Dependencies | Large C++/Python ecosystem | Pure Rust, `rusb` + `eframe` |
| Sensor support | Broad (all Prophesee sensors) | Focused (EVK4 / IMX636) |
| Recording path | Shared with analysis layer | Isolated 3-thread pipeline |
| Live analysis | Separate ecosystem | Plugin system in the same binary |
| Audit surface | Large | Small, readable |
| Config | Custom formats | TOML with automatic sidecar |
| Reproducibility | Manual | Built in |

---

## Platform Support

| Platform | Status |
|---|---|
| macOS | Primary — hardware-tested |
| Linux | CI-verified on every push |
| Windows | CI-verified on every push |

Tagged releases ship a CLI archive and an unsigned macOS `.app` bundle.

---

## Documentation

| Document | Description |
|---|---|
| [Getting Started](./docs/getting-started.md) | Build, connect, first capture |
| [Configuration](./docs/configuration.md) | TOML reference: biases, ROI, mask, filters |
| [CLI Reference](./docs/cli.md) | Commands and scripting |
| [GUI Guide](./docs/gui.md) | Live preview, controls, and the plugin panel |
| [Recording Format](./docs/recording.md) | EVT3 output and pipeline behavior |
| [Performance](./docs/performance.md) | Architecture and design rationale |
| [Plugin Architecture](./docs/features/analysis-plugins.md) | Plugin API: input kinds, phases, context bus |
| [Technical Notes](./docs/features/README.md) | SDK internals, ROI grid, and feature details |
| [Architecture Decisions](./docs/adr/README.md) | ADRs |

---

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md), [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md), and [SECURITY.md](./SECURITY.md).

Plugin contributions go to the [augur-plugins](https://github.com/muthmann/augur-plugins) repository.

---

## License

[MIT](./LICENSE)
