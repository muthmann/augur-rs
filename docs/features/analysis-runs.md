# Analysis Runs

## Summary

Analysis runs are the primary way to compute plugin results: deterministic,
range-based, background analysis with full provenance. Live plugin output in
the investigation workspace is a preview; runs hold the exact, exportable
results. See ADR 025 for the decision and ADR 022 for the underlying
offline pipeline.

## Workflow

1. **Configure** — `Analysis ▸ New Analysis…` opens the run dialog:
   - Input file: pre-filled with the open replay; any replay file can be
     picked, and its timeline is probed on a background thread.
   - Range: whole file, or a custom `[start, end)` time range with sliders.
     When the replay is open, "Start from playhead" / "End at playhead"
     quick-set buttons anchor the range to the displayed frame. A range whose
     end slider sits at the file end includes the final event.
   - Window: fixed analysis window in ms (independent of the 2D preview's
     acquisition time — run windows are anchored to timestamps, like the 3D
     look-back window).
   - Plugins: checkbox list seeded from current enablement; selected plugins
     run with their current GUI settings, snapshotted at start.
   - Run name and output folder (a slug of the name becomes the folder).
2. **Run** — executes on a background thread through `run_offline_analysis`;
   the Analysis Runs panel shows progress and supports cancel. One run
   executes at a time. Exports are atomic (temp dir, renamed on success).
3. **Inspect** — on completion the run's host-view snapshots are published to
   the workspace (unless playback is actively streaming); "View results"
   re-publishes them at any time and pauses replay first. CSV/PNG/JSON
   exports land in the run's output folder ("Open folder").

The runner fails closed if any configured plugin that is not explicitly
disabled is missing, ABI-incompatible, or otherwise failed to load. It never
reports a successful run that silently omitted a requested analysis plugin.

## Provenance

The workspace inspector shows a **Data source** badge naming what it
displays:

- **Live preview ≈** — approximate worker output; cadence can lag under load
- **run name** — exact results of a finished analysis run (range + window in
  the tooltip)
- **Current frame** — synchronous recompute for the displayed frame while
  replay is paused
- **No plugin data** — nothing computed yet

Removing a run that is currently shown clears the workspace back to live
data. Run results live for the session; the exported files are the durable
artifact.

## CLI parity

`augur-cli analyze <file> --config <toml> --out <dir>` accepts
`--t-start-us` / `--t-end-us` overrides, and the config file supports
`t_end_us`. GUI-started runs use the same config semantics, so a run is
reproducible from the CLI. (The CLI's Ctrl+C flag previously had inverted
polarity and cancelled every run immediately; fixed alongside this feature.)

## Files

| File | Role |
|---|---|
| `augur-gui/src/analysis_runs.rs` | run dialog, runs panel, run lifecycle |
| `augur-gui/src/app.rs` | Analysis menu, run start/poll/publish, data-source badge |
| `augur-runtime/src/offline.rs` | `t_end_us` range clamp, `probe_replay_file`, `json_value_to_toml` |
| `augur-cli/src/main.rs` | range flags, corrected cancel flag |

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo test --workspace --locked` (range clamp, empty-range rejection,
  required-plugin validation, probe bounds, dialog request building, config
  parsing)
- End-to-end: `augur-cli analyze` on a synthetic CSV recording with
  `--t-start-us 10 --t-end-us 20` produces exactly the `[10, 20)` window;
  the whole-file run covers `[10, 41)`.
- Manual GUI pass recommended: dialog flow, progress/cancel, badge states.
