# RAW Replay

## Summary

AugurRS can replay recorded EVT3 `.raw` files through the same preview and plugin pipeline used for live camera sessions. Replay mode opens a file-backed `PacketStreamCamera`, decodes frames normally, and exposes transport controls in the GUI for play/pause, stepping, speed, and progress.

## Core Design

Replay lives in `augur-core` as `RawFileCamera` so the rest of the stack can treat file playback like any other packet-stream camera.

- parses the EVT3 header (`% format`, `% geometry`, `% end`) before streaming
- records the replay data offset and source geometry
- reads packets from disk through `PacketStreamCamera::read_packet`
- returns `CameraError::Timeout` while paused
- returns `CameraError::Eof` at end-of-file so the pipeline can stop cleanly

`ReplayControls` is a shared handle between the GUI and the replay camera:

- `paused`
- `speed_bits`
- `bytes_read`
- `file_size`
- `data_offset`
- `width`
- `height`

The current implementation throttles replay with a byte-rate model derived from the file's decoded timestamp span. It is intentionally approximate and can be refined later to use packet- or event-level timing.

## GUI Workflow

1. Click `Open .raw` from the toolbar while the app is idle.
2. AugurRS starts a preview-only pipeline using `RawFileCamera`.
3. The center panel switches to `Replay` mode and shows:
   - `Play` / `Pause`
   - `Step Forward` when paused
   - replay speed selection (`0.25x` through `Max`)
   - a progress bar based on bytes consumed from the file
4. Enabled analysis plugins continue to run on replayed frames exactly as they do on live preview frames.

If a `<capture>.toml` sidecar exists next to the selected `.raw` file, the settings panel shows it as read-only reference data during replay.

## EOF Behavior

Replay EOF is treated as a normal shutdown path, not as a pipeline error:

- the USB/read thread stops when `RawFileCamera` returns `CameraError::Eof`
- queued preview bytes drain before the preview thread exits
- the GUI returns to idle automatically when replay finishes

## Files

| File | Role |
|---|---|
| `augur-core/src/replay.rs` | `RawFileCamera`, `ReplayControls`, EVT3 header parsing, replay throttling |
| `augur-core/src/error.rs` | `CameraError::Eof` |
| `augur-core/src/pipeline.rs` | Clean EOF handling and preview-queue drain on shutdown |
| `augur-gui/src/app.rs` | Replay mode, file picker, transport controls, EOF-to-idle transition |

## Verification

- `cargo build --workspace`
- `cargo test --workspace`
