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
cadence, not upstream events; host-authored persistent-bus updates from
superseded jobs are merged into the surviving job. If retained history falls
behind the ring capacity, the worker emits a warning, clears plugin history,
and sends an explicit `HistoryEvicted` discontinuity.

The controller-owned lossless `plugin-runtime` cursor is drained by the GUI on
every dequeued frame batch — including while the worker executes plugins. A
registered lossless cursor that never advances blocks ring eviction and stalls
raw-event archival once the ring wraps. The GUI registers that cursor on
demand when a retained-history plugin is enabled mid-session and releases it
when no plugin needs history; the worker's own history never unregisters a
cursor it did not register.

## Persistent Context Bus Ownership

The worker's persistent map is authoritative between jobs, so
plugin-published persistent values are never rolled back by a stale GUI echo.
Jobs carry either a full bus snapshot (`persistent_seed` — sent at startup,
after an epoch bump, and after the synchronous GUI executor may have written
plugin values) or an incremental set of host-authored upserts/removals
(`persistent_updates`). `reset_analysis` clears both sides through an
epoch-tagged `ClearPersistent` command so pre-reset (possibly cross-source)
values cannot resurrect through in-flight results.

Unpaused replay playback uses the live worker for dynamic plugins. Paused replay
scrubs and explicit single-frame recomputes use the synchronous path so the
displayed frame and plugin result are exact and immediate.

Live host-view registries and dataset payloads are snapshotted by the worker and
published with the analysis result. The GUI resolves and renders those snapshots
through the existing host-view UI.

## Offline Pipeline

`augur-cli analyze <file> --config <toml> --out <dir>` and GUI analysis runs
(`Analysis ▸ New Analysis…`, see `analysis-runs.md` and ADR 025) both call
`run_offline_analysis` in `augur-runtime`. Runs are the primary analysis
workflow; the live worker output is a preview.

The offline runner:

- opens raw and decoded event files through the core replay sources
- loads runtime plugins and applies all configured settings before the first
  window
- processes deterministic half-open timestamp windows over `[t_start, t_end)`
  (both bounds optional; `t_end_us` clamps the final window)
- feeds empty windows to accumulating plugins
- emits a final partial window instead of dropping tail events
- exports TableV1 datasets as CSV, Image2dV1 datasets as PNG, and Series1dV1
  datasets as JSON
- publishes the final plugin host-view snapshots back to the GUI action so the
  existing host-view UI can inspect the completed offline result
- writes to a temporary directory and renames it only after a successful run

The config format supports top-level `t_start_us`, `t_end_us`, `acq_time_us`
or `acq_time_ms`, plus per-plugin entries (`augur-cli analyze` additionally
accepts `--t-start-us` / `--t-end-us` overrides):

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
return `PluginStateKind::Stateless` and skip accumulator resets. The host
delivers `on_discontinuity` to every loaded plugin; the default trait
implementation performs the state-kind filtering, so stateless plugins that
override the hook still observe timeline boundaries.

Raw-event visibility is identical across live, paused-scrub, and offline
paths: only `RawEvents`-phase plugins receive the current frame's raw events.
A plugin's own input-kind declaration — never another plugin's needs —
decides what it sees, so live results agree with whole-file runs.

Discontinuities are emitted for seek, source change, settings change, and
retained-history eviction. Replay seeks and source replacements clear
`PluginEventHistory` before new frames are analyzed, preventing pre-seek and
post-seek windows from sharing one monotonic history deque.

`PluginEventHistory` also enforces that ordering itself, so a session boundary
that reaches the worker without a discontinuity command cannot interleave two
timelines: attaching a different upstream source drops the previous source's
frames, and a frame whose window opens before the newest retained one drops the
pre-jump history.

## Hot-Path Rules

Preview accumulation retains raw events only when `raw_events_needed` is set on
the active `PipelineController`. That signal includes:

- 3D point-cloud or raw-event preview consumers
- enabled `RawEvents` plugins
- enabled plugins that request retained event history

Enabled `FrameOnly` and `DerivedData` plugins no longer force current-frame raw
event materialization. Ring-backed frames materialize `FfiCdEvent` directly from
`CompactEvent` storage, and `EventStoreHandle::frame_at` requests share one
materialization cache per analysis pass — N retained-history plugins decode
each history frame once, not N times.

Host-view dataset payloads are fetched and JSON-decoded on the worker thread,
gated by provider generation: unchanged datasets ship as `Arc` clones of the
previous decode instead of a serialize/parse round-trip per frame. The GUI
consumes decoded payloads directly and only re-resolves the host-view
registry when a result's registries actually changed.

Global settings JSON is cached until the effective settings change.

## Verification

The implementation is covered by focused tests for raw-event gating,
ring-backed event materialization, retained-history caching, ABI v4 layout, and
offline windower/config parsing. End-to-end verification should include the full
workspace test suite plus a manual run of live plugins and `augur-cli analyze` on
a representative recording.
