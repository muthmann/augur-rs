# AugurRS Technical Notes

## Goal

macOS-compatible Rust toolkit for Prophesee EVK4 / IMX636 with:

- trait-based camera abstraction
- headless CLI recording
- egui GUI preview and runtime controls

## Current Technical Scope

The current implementation includes:

- Treuzell USB transport over `rusb` with control and stream endpoints split
- Treuzell command framing and property handling
- IMX636 sensor integration with sourced register addresses and ISSD init/start/stop/destroy sequences
- runtime controls for biases, ROI, pixel mask, and digital filters
- three-thread recording pipeline with bounded backpressure and lossy preview
- runtime acquisition window (`acq_time_us`) via `Arc<AtomicU64>`
- GUI-thread live analysis framework with extensible analyzers
- hotpixel detection with EMA smoothing, preview overlays, severity warnings, and mask-copy handoff

## Architecture

- `augur-core`: traits, config types, pipeline, and `evt3-core`-backed incremental EVT3 preview decoding
- `augur-prophesee`: EVK4 transport, Treuzell protocol, IMX636 sensor implementation
- `augur-cli`: status, config, and record commands
- `augur-gui`: live preview and runtime controls

## Data Path Guarantees

- Disk path is bounded and backpressured.
- Preview path is lossy and never blocks the USB hot path.
- GUI throughput stats use a 1-second sliding window for current `Mev/s` and `MB/s`.
- Worker errors are surfaced through a pipeline error channel to CLI and GUI clients.
- USB stream timeouts are treated as non-fatal in low-event scenes.

## Sensor-Specific Behavior

- ROI is configured in hardware and reduces event generation bandwidth.
- The digital event mask uses IMX636 hardware mask slots with a 64-pixel budget.
- STC and Trail filters are configured from IMX636 registers and treated as mutually exclusive.
- Biases are applied as offsets from factory defaults with range checks.

## Recording Format

Every recording writes a sibling TOML file with the full effective configuration so captures remain reproducible.
