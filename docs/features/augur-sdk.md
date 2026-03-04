# AugurRS Technical Notes

## Goal
macOS-compatible Rust toolkit for Prophesee EVK4 / IMX636 with:
- trait-based camera abstraction
- headless CLI recording
- egui GUI preview and runtime controls

## Current Technical Scope

The current implementation includes:
- Treuzell USB transport over `rusb` with control/stream endpoints split
- Treuzell command framing and property handling (devices, enable, stream, reg32, format)
- IMX636 sensor integration with sourced register addresses and ISSD init/start/stop/destroy sequences
- runtime controls for biases, ROI, pixel mask and digital filters
- three-thread recording pipeline with bounded backpressure + lossy preview
- runtime acquisition window (`acq_time_us`) via `Arc<AtomicU64>`
- GUI-thread live analysis framework with extensible analyzers
- hotpixel detection with EMA smoothing, preview overlays, severity warnings, and mask-copy handoff
- CLI + GUI both consume the same pipeline and camera backend

## Architecture
- `augur-core`: traits, config types, pipeline, and `evt3-core`-backed incremental EVT3 preview decoding
- `augur-prophesee`: EVK4 transport, Treuzell protocol, IMX636 sensor implementation
- `augur-cli`: status/config/record commands
- `augur-gui`: live preview + controls

## Data Path Guarantees
- Disk path is bounded and backpressured (`disk_tx`): avoids unbounded memory growth.
- Preview path is lossy (`try_send`): never blocks USB hot path.
- GUI throughput stats use a 1-second sliding window for current `Mev/s` and `MB/s`; elapsed time remains since pipeline start.
- Worker errors are surfaced through a pipeline error channel to CLI/GUI.
- USB stream timeouts are treated as non-fatal (low-event scenes are valid).
- Preview analysis runs on the GUI thread against the latest `PreviewFrame`; no extra analysis thread or channel is required for current workloads.

## Sensor-Specific Behavior (IMX636)
- ROI is configured in hardware (`roi_win_*`, `roi_ctrl`) and reduces event generation bandwidth.
- Digital event mask uses hardware mask slots (`digital_mask_pixel_00..63`, max 64 masked pixels).
- STC and Trail filters are configured from IMX636 `stc/*` registers and treated as mutually exclusive.
- Biases are applied as offsets from factory defaults with range checks.

## Recording Format
`.raw` files start with EVT3 header lines and always use sensor geometry:
- `% format EVT3;width=1280;height=720`
- `% geometry 1280x720`
- `% evt 3.0`
- `% end`

Each recording writes a sibling TOML (`<name>.toml`) with full configuration.

## Configuration
- Supports `masked_pixels` and backward-compatible alias `hot_pixels` in TOML.
- GUI analysis settings are local to the desktop app and do not change hardware state until the user copies detections into the DEM mask and applies runtime settings.
- Runtime validation checks:
  - ROI bounds
  - mask coordinates
  - digital filter conflicts (`stc_enabled` and `trail_enabled` cannot both be true)
