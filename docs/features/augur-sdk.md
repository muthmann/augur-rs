# AugurRS Technical Overview

## What It Is

A Rust workspace for controlling Prophesee EVK4 / IMX636 event cameras on macOS. It provides direct USB communication, sensor register programming, a bounded streaming pipeline, and both CLI and GUI frontends.

## Workspace Structure

| Crate | Role |
|-------|------|
| `augur-core` | Camera traits, TOML config types, streaming pipeline, EVT3 preview decoding (via [`evt3-core`](https://crates.io/crates/evt3-core/0.1.0)), analysis framework |
| `augur-prophesee` | EVK4 Treuzell USB transport, IMX636 register sequences (ISSD init/start/stop/destroy), sensor trait implementation |
| `augur-cli` | `status`, `record`, `config` commands |
| `augur-gui` | Live preview, runtime controls, hotpixel analysis, ROI-grid computation |

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
- Preview analysis runs on the GUI thread against the latest decoded frame

## Recording Output

Each capture produces:
- `<name>.raw` — EVT3 stream with standard header (`format EVT3;width=1280;height=720`)
- `<name>.toml` — effective configuration sidecar for reproducibility

## Configuration

TOML-based with four sections: `biases`, `roi`, `pixel_mask`, `digital_filter`. The GUI and CLI share the same config types. Runtime validation covers ROI bounds, mask coordinates, and filter conflicts.
