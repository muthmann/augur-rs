# RAW Replay

## Summary

AugurRS can replay recorded EVT3 `.raw` files through the same preview and plugin pipeline used for live camera sessions. Replay now opens large files with a constant-time metadata scan, keeps the last frame visible at EOF, and supports restart and seek operations without leaving replay mode.

## Core Design

Replay lives in `augur-core` as `RawFileCamera`, so the rest of the stack can treat file playback like any other packet-stream camera.

- parses the EVT3 header (`% format`, `% geometry`, `% end`) before streaming; tolerates older files that omit `% format` or contain non-UTF-8 header bytes
- records the replay data offset and source geometry once in `ReplayFileInfo`
- scans only the first and last replay windows to estimate total duration and nominal byte rate
- reopens at an arbitrary byte offset through `RawFileCamera::open_at`
- returns `CameraError::Timeout` while paused
- returns `CameraError::Eof` at end-of-file so the pipeline can stop cleanly

`ReplayControls` is the shared handle between the GUI and the replay camera:

- `paused`
- `speed_bits`
- `bytes_read`
- `file_size`
- `data_offset`
- `width`
- `height`
- `total_duration_us`
- `first_timestamp_us`

The current replay pacing remains approximate: it uses a byte-rate model derived from the file's timestamp span, not per-event scheduling.

## GUI Workflow

1. Click `Open .raw` from the toolbar while the app is idle.
2. AugurRS opens the file with `RawFileCamera::open` and starts a preview-only pipeline.
3. The center panel switches to `Replay` mode and shows:
   - `Play` / `Pause`
   - `Restart`
   - replay speed selection (`0.25x` through `Max`)
   - a timeline slider for seeking
   - current / total replay time
   - MB progress as secondary info
4. Enabled analysis plugins continue to run on replayed frames exactly as they do on live preview frames.

If a `<capture>.toml` sidecar exists next to the selected `.raw` file, the settings panel shows it as read-only reference data during replay.

## EOF Behavior

Replay EOF is treated as a normal shutdown path, not as a pipeline error:

- the USB/read thread stops when `RawFileCamera` returns `CameraError::Eof`
- queued preview bytes drain before the preview thread exits
- the GUI shuts down the controller threads but stays in replay mode
- the final frame, replay metadata, and transport controls remain visible
- the user can restart, seek somewhere else, or close replay with `Stop`

## Seek Behavior

Seeking reopens the replay camera at an aligned byte offset:

- target offsets are computed from the replay data length and requested fraction
- offsets are aligned to EVT3 word boundaries relative to the data start
- the decoder naturally resynchronizes from nearby time words after reopen

This is intentionally lightweight and fast rather than perfectly timestamp-aware.

## Files

| File | Role |
|---|---|
| `augur-core/src/replay.rs` | `RawFileCamera`, `ReplayFileInfo`, fast metadata scan, reopen-at-offset support |
| `augur-core/src/error.rs` | `CameraError::Eof` |
| `augur-core/src/pipeline.rs` | Clean EOF handling and preview-queue drain on shutdown |
| `augur-gui/src/app.rs` | Replay mode, persisted EOF state, transport controls, seeking, restart |

## Older File Compatibility

Older Prophesee `.raw` files (e.g. recorded with earlier MetaVision SDK versions) may differ from the current header convention in three ways:

1. **Non-UTF-8 header bytes** — the header parser uses `read_until` + `from_utf8_lossy` so stray binary bytes are tolerated.
2. **Missing `% format` line** — if only `% geometry WxH` is present, the parser proceeds and assumes EVT3. A `% format` line that declares a non-EVT3 codec is still rejected.
3. **Truncated EVT3 stream** — `finish_stream()` errors during the fast timestamp scan are silenced; whatever timestamps were decoded before the unexpected EOF are used as-is.

## Verification

- `cargo build --workspace`
- `cargo test --workspace`
