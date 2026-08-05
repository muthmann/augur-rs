# ADR 022: Analysis Runtime, Live Worker, And Offline Pipeline

## Status

Accepted.

## Context

Dynamic runtime plugins were executed synchronously from the egui update path.
One slow plugin could therefore stall input, repaint, preview rendering, and
other UI work. The same code path also mixed live-preview behavior with
deterministic whole-file analysis requirements.

Replay scrubbing had a separate correctness issue: retained plugin history could
span pre-seek and post-seek timelines, while timestamp range lookup assumes a
monotonic history deque.

## Decision

Introduce `augur-runtime` as the egui-free crate that owns dynamic plugin
loading, FFI bridges, retained plugin history, live worker execution, and
offline whole-file orchestration.

Live analysis uses one dedicated worker thread. The worker owns its plugin
instances and retained-history cursor; the GUI keeps a separate plugin manager
as the settings/status/UI mirror and synchronous replay/scrub executor. The GUI
sends epoch-tagged configuration and frame jobs to the worker. The worker may
coalesce pending frame triggers, but it drains retained history through its
cursor before processing, so coalescing reduces result cadence rather than
dropping upstream events. Unpaused replay playback uses the worker; paused
single-frame scrubs stay synchronous.

Offline analysis uses a deterministic timestamp windower anchored at configured
`t_start`. Windows are half-open `[start, end)`, empty windows are presented to
accumulating plugins, and the final partial window is emitted through
`last_event_ts + 1`. The CLI and GUI call the same `run_offline_analysis` entry
point.

Plugin ABI v4 adds `PluginDiscontinuity` and `PluginStateKind`. Accumulating
plugins receive discontinuities on seek, source change, settings change, and
retained-history eviction. The host clears retained history at discontinuity
boundaries.

## Consequences

- Live GUI responsiveness no longer depends directly on dynamic plugin frame
  cost.
- Live results are approximate because slow plugins can publish less often than
  preview frames arrive.
- Offline results are deterministic and exportable without replay-clock timing.
- Stale live results are dropped by epoch before they reach host views or
  overlays.
- Plugins must be rebuilt against ABI v4.
- Future plugin UI work must remember that GUI-side plugin instances are a
  configuration mirror for live analysis; live and unpaused replay playback
  results come from worker snapshots, while paused replay recompute results come
  from the GUI manager.
