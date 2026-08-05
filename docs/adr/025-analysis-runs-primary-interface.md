# ADR 025: Analysis Runs As The Primary Analysis Interface

## Status

Accepted.

## Context

ADR 022 split plugin execution into a live worker (approximate, best-effort)
and a deterministic offline pipeline. The product surface did not reflect that
split: the offline pipeline was a single "Analyze Whole File…" menu item with
no range selection, no plugin configuration, and no record of what was
computed, while live worker output filled the investigation workspace and
could be mistaken for exact results.

Scientific usage splits by phase: during acquisition users need monitoring
(focus, event rate, hotpixels), during exploration they scrub and want to
analyze a selected segment, and for publishable results they need determinism
and provenance (file, range, window, plugin versions and settings). Many
analyses are also non-causal — classifying an event needs later events — so a
streaming path can only ever produce provisional values.

The 2D preview and 3D point cloud already use independent look-back windows
sharing one right-edge timestamp; plugin windows tied to the 2D acquisition
time were the odd one out.

## Decision

Make deterministic, provenance-tagged **analysis runs** the primary analysis
workflow, and demote live plugin output to a clearly-labeled preview.

- An analysis run captures its full configuration: input file, half-open
  timestamp range `[t_start, t_end)`, window length, and the selected plugins
  with their settings snapshotted from the GUI at start.
- `OfflineAnalysisConfig` gains `t_end_us`; the windower clamps to it. The
  CLI accepts `--t-start-us` / `--t-end-us`. GUI and CLI share
  `run_offline_analysis` unchanged.
- The GUI gets a top-level Analysis menu, a run-configuration dialog (range
  selection with playhead quick-set, plugin selection seeded from current
  enablement, window, output folder), and an Analysis Runs panel that keeps
  each run's status, configuration summary, and results for the session.
- Run windows are configured per run, anchored to timestamps — not to the 2D
  preview's acquisition time. This generalizes the existing "N windows, one
  anchor" contract of the 2D/3D split.
- The workspace names its data source explicitly: a badge distinguishes
  "Live preview ≈" (worker), a run's name (exact results), and "Current
  frame" (synchronous paused-scrub recompute). Run results are published to
  the workspace on completion only when playback is not actively streaming;
  viewing a run pauses replay first so live results do not overwrite it.
- One run executes at a time; runs are cancellable and export atomically
  (temp-dir rename, from ADR 022).

## Consequences

- Exact results now have an owner the user can point at: a named run with its
  configuration, instead of "whatever the workspace currently shows".
- Live output is visibly a preview; the epoch/discontinuity machinery and the
  live worker are unchanged.
- Plugin settings travel through the shared TOML config
  (`json_value_to_toml`), so a GUI-started run is reproducible from the CLI
  with the same config file.
- Run results are session-scoped; the exported files on disk are the durable
  artifact.
- Deferred follow-ups: replay browsing a run's precomputed result at the
  current timestamp (needs a per-timestamp result index), windower lookahead
  for non-causal plugins, and a `live_preview` capability flag in plugin
  manifests. The paused-scrub synchronous recompute path (ADR 022/024)
  remains and can be retired once replay browses runs.
