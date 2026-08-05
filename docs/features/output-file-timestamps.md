# Output File Timestamps

Prevents accidental overwrite of existing recordings and provides automatic timestamp-based file naming.

## Behavior

### Default filename

The default output path is `./output_YYYYMMDD_HHMMSS.raw` using UTC time at application startup. The Browse dialog also defaults to a timestamped filename.

### Overwrite protection

When a recording starts, `validated_output_path()` checks whether the resolved file already exists. If it does, a `_YYYYMMDD_HHMMSS` suffix is inserted before the extension so the existing file is never overwritten.

### "Always add timestamp" checkbox

A checkbox below the output path input lets the user opt into unconditional timestamping. When enabled, every recording gets a fresh `_YYYYMMDD_HHMMSS` suffix appended to the stem — regardless of whether the file already exists. This is useful for batch recording sessions where the user keeps the same base name.

Both the checkbox and the output path input are disabled while a recording is active.

### The displayed destination is the resolved one

Because both mechanisms above rename the target, the path in the output field is a *request*, not
the answer. Every place that names the destination reads the path the pipeline actually opened:

- the top-bar status chip: `Recording · output_20260801_101500.raw`
- the capture card: a `Writing to <path>` line appears whenever the resolved path differs from the
  configured one, with hover text saying why
- the sensor-telemetry companion filename preview
- the idle message after the recording stops: `Camera idle. Last recording written to <path>.`

## Implementation

All timestamp logic lives in `augur-gui/src/app.rs`:

- `format_timestamp_now()` — formats current UTC time as `YYYYMMDD_HHMMSS` using only `std::time::SystemTime` (no external dependency).
- `civil_from_days()` — epoch-day to calendar-date conversion (Howard Hinnant algorithm).
- `insert_timestamp_suffix()` — splices `_YYYYMMDD_HHMMSS` between the file stem and extension.
- `validated_output_path()` — applies the always-timestamp and exists-check logic before the path reaches the pipeline.
- `CameraApp::active_recording_path` — the resolved path, set by `begin_recording` and taken by
  `stop_pipeline_with_failure`.
- `recording_target_name()` — the file name shown for a running recording, derived only from the
  resolved path.

The `CameraApp` struct gained an `always_timestamp: bool` field (default `false`).

See ADR 033 for why the resolved path, not the configured one, is the one on screen.
