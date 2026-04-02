# AugurRS Technical Overview

## What It Is

A Rust workspace for controlling Prophesee EVK4 / IMX636 event cameras on macOS. It provides direct USB communication, sensor register programming, a bounded streaming pipeline, and both CLI and GUI frontends.

## Workspace Structure

| Crate | Role |
|-------|------|
| `augur-core` | Camera traits, TOML config types, streaming pipeline, EVT3 preview decoding (via [`evt3-core`](https://crates.io/crates/evt3-core/0.2.0)), analysis framework |
| `augur-prophesee` | EVK4 Treuzell USB transport, IMX636 register sequences (ISSD init/start/stop/destroy), sensor trait implementation |
| `augur-cli` | `status`, `record`, `config` commands |
| `augur-gui` | Live preview, runtime controls, built-in ROI-grid tooling, and the dynamic plugin host |

## Hardware Interface

- **Transport**: Treuzell protocol over `rusb` — control and stream endpoints, command framing, property queries (device info, enable, stream, reg32, format)
- **Sensor**: IMX636 register programming with sourced addresses. Init/start/stop/destroy sequences derived from ISSD specifications
- **Runtime controls**: Biases (5 offset keys), hardware ROI, 64-slot digital event mask (DEM), mutually exclusive STC/Trail filters, acquisition window via `Arc<AtomicU64>`

## Streaming Pipeline

Three threads with explicit flow control:

| Thread | Channel | Behavior |
|--------|---------|----------|
| USB reader | — | Pulls raw EVT3 packets from the device |
| Disk writer | Bounded (`disk_tx`) | Backpressure prevents unbounded memory growth |
| Preview decoder | Lossy (`try_send`) | Drops frames rather than blocking capture |

Additional guarantees:
- Throughput stats use a 1-second sliding window (Mev/s, MB/s)
- Worker errors surface through a dedicated error channel
- USB timeouts are non-fatal (low-event scenes are valid)
- Recording output is opened and header-initialized before streaming starts
- Preview analysis still runs on the GUI thread, but only against the latest processed preview frame and at a capped UI cadence

## Recording Output

Each capture produces:
- `<name>.raw` — EVT3 stream with configured geometry plus recorded device identity, software provenance, and pixel pitch
- `<name>.toml` — effective configuration sidecar with a `[metadata]` table for provenance, timing, and optional annotations

## Configuration

TOML-based with five sections: `biases`, `roi`, `pixel_mask`, `digital_filter`, and `global`.

- `biases`, `roi`, `pixel_mask`, and `digital_filter` capture live camera programming state
- `global` persists host-owned settings such as pixel scale, configured sensor geometry,
  acquisition time, EventStore budget, preview cadence, point-cloud cadence, and disk-writer
  buffer size

The GUI and CLI share the same config types. Runtime validation still covers ROI bounds, mask
coordinates, and filter conflicts, and older TOML files without `[global]` continue to load via
defaults.
