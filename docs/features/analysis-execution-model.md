# Analysis Execution Model

## Summary

Augur now has two explicit analysis paths that share the egui-free
`augur-runtime` crate:

- **Live analysis** runs dynamic plugins on a dedicated worker thread so the GUI
  tick stays responsive. Live results are labeled approximate because result
  cadence can lag under load.
- **Offline analysis** runs the same plugin phases over a whole file with a
  deterministic timestamp windower, then exports plugin host-view datasets.

The built-in hotpixel tool remains host-owned in `augur-gui`; dynamic plugin
loading, retained event history, host context callbacks, and offline analysis
orchestration live in `augur-runtime`.

## Live Worker

`CameraApp` keeps a GUI-side plugin manager for settings, status, and plugin
manager UI. A second plugin manager is loaded inside `LiveAnalysisWorker` and
owns the plugin instances used for live processing.

This dual-manager setup is intentional. The GUI manager is the configuration
mirror and the synchronous replay/scrub executor. The worker manager is the live
executor. Mode transitions must treat the two managers as mode-exclusive state
owners and mirror GUI settings to the worker through epoch-tagged configuration
messages before live results are accepted.

Whenever plugin enablement, settings, global settings, source identity, or seek
state changes, the GUI bumps an analysis epoch and sends the worker a fresh
configuration or discontinuity command. Worker results carry that epoch; the GUI
drops stale results before publishing them to the investigation workspace.

The worker coalesces pending frame triggers by keeping the newest queued frame,
but retained-history plugins drain all available frames from their own
`LiveEventSource` cursor before processing. Coalescing therefore drops result
cadence, not upstream events. If retained history falls behind the ring capacity,
the worker emits a warning, clears plugin history, and sends an explicit
`HistoryEvicted` discontinuity to accumulating plugins.

Unpaused replay playback uses the live worker for dynamic plugins. Paused replay
scrubs and explicit single-frame recomputes use the synchronous path so the
displayed frame and plugin result are exact and immediate.

Live host-view registries and dataset payloads are snapshotted by the worker and
published with the analysis result. The GUI resolves and renders those snapshots
through the existing host-view UI.

## Offline Pipeline

`augur-cli analyze <file> --config <toml> --out <dir>` and the GUI
`Analyze Whole File...` action both call `run_offline_analysis` in
`augur-runtime`.

The offline runner:

- opens raw and decoded event files through the core replay sources
- loads runtime plugins and applies all configured settings before the first
  window
- processes deterministic half-open timestamp windows
- feeds empty windows to accumulating plugins
- emits a final partial window instead of dropping tail events
- exports TableV1 datasets as CSV, Image2dV1 datasets as PNG, and Series1dV1
  datasets as JSON
- publishes the final plugin host-view snapshots back to the GUI action so the
  existing host-view UI can inspect the completed offline result
- writes to a temporary directory and renames it only after a successful run

The config format supports top-level `t_start_us`, `acq_time_us` or
`acq_time_ms`, plus per-plugin entries:

```toml
t_start_us = 0
acq_time_ms = 1

[plugins."Example Plugin"]
enabled = true

[plugins."Example Plugin".settings]
threshold = 12.5
mode = "fast"
```

## ABI v4 Lifecycle

`augur-plugin-api` ABI v4 adds:

- `PluginDiscontinuity`
- `PluginStateKind`
- `Plugin::on_discontinuity`
- `Plugin::plugin_state_kind`

Accumulating plugins default to resetting on discontinuity. Stateless plugins can
return `PluginStateKind::Stateless` and skip accumulator resets.

Discontinuities are emitted for seek, source change, settings change, and
retained-history eviction. Replay seeks and source replacements clear
`PluginEventHistory` before new frames are analyzed, preventing pre-seek and
post-seek windows from sharing one monotonic history deque.

## Hot-Path Rules

Preview accumulation retains raw events only when `raw_events_needed` is set on
the active `PipelineController`. That signal includes:

- 3D point-cloud or raw-event preview consumers
- enabled `RawEvents` plugins
- enabled plugins that request retained event history

Enabled `FrameOnly` and `DerivedData` plugins no longer force current-frame raw
event materialization. Ring-backed frames materialize `FfiCdEvent` directly from
`CompactEvent` storage, and repeated `EventStoreHandle::frame_at` requests reuse
a per-call cache.

Global settings JSON is cached until the effective settings change.

## Verification

The implementation is covered by focused tests for raw-event gating,
ring-backed event materialization, retained-history caching, ABI v4 layout, and
offline windower/config parsing. End-to-end verification should include the full
workspace test suite plus a manual run of live plugins and `augur-cli analyze` on
a representative recording.
