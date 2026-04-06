# Replay

## Summary

AugurRS can replay recorded EVT3 `.raw` files plus decoded `.csv`, `.bin`, `.npy`, and optional `.h5` / `.hdf5` event files through the same preview and plugin pipeline used for live camera sessions. Replay keeps the last frame visible at EOF, supports restart and seek operations without leaving replay mode, restores recorded raw-file metadata when present, resets its pacing baseline cleanly when speed changes, supports keyboard play/pause and frame stepping, and finalizes older `.raw` files without suppressing genuine preview errors.

## Core Design

Replay lives in `augur-core` as two file-backed camera adapters, so the rest of the stack can treat file playback like any other packet-stream camera:

- `RawFileCamera` parses the EVT3 header (`% format`, `% geometry`, `% evt`, metadata lines, `% end`), tolerates older files that omit `% format` or contain non-UTF-8 header bytes, scans only the first and last replay windows to estimate duration/byte rate, and reopens at arbitrary EVT3-aligned offsets through `RawFileCamera::open_at`
- `DecodedEventFileCamera` parses decoded `.csv`, `.bin`, `.npy`, and optional `.h5` / `.hdf5` event files once, keeps the decoded `CdEvent` vector in shared memory for cheap seeks, and replays those events through an internal packed 14-byte transport consumed by `PackedEventPreviewDecoder`
- `.raw` replay stores parsed header metadata in `ReplayFileInfo` and uses it to rebuild replay `DeviceInfo`
- both cameras record geometry and replay timing in `ReplayFileInfo`
- both return `CameraError::Timeout` while paused
- both return `CameraError::Eof` at end-of-file so the pipeline can stop cleanly

`ReplayControls` is the shared handle between the GUI and the replay camera:

- `paused`
- `speed_bits`
- `speed_epoch`
- `bytes_read`
- `current_timestamp_us`
- `file_size`
- `data_offset`
- `width`
- `height`
- `total_duration_us`
- `first_timestamp_us`

Replay pacing is now timestamp-driven instead of byte-rate-driven:

- raw EVT3 replay updates `ReplayControls::current_timestamp_us` directly from the packet reader
- decoded replay reads the current timestamp directly from its cached event vector
- speed changes and seeks reset the local pacing baseline from the current timestamp rather than
  from bytes read or event count

That keeps `1x` much closer to recorded event time even when event density varies across the file.

## Decoded File Formats

The decoded replay path is intentionally narrow and matches the evt3-core decoder output formats:

- `.csv` — XYPT rows with a required `%geometry:W,H` header
- `.bin` — `EVT3BIN\0` header + 14-byte packed XYPT records
- `.npy` — structured NumPy array with fields `x`, `y`, `p`, padding, `t`; geometry is inferred from the event bounds with a minimum of `1280x720`
- `.h5` / `.hdf5` — Prophesee HDF5 recordings using the `CD/events` dataset; geometry comes from the file-level `geometry` attribute when present, whether it is stored as variable-length ASCII or variable-length Unicode, otherwise it is inferred from event bounds with a minimum of `1280x720`

HDF5 replay support is compiled behind the `hdf5` feature on `augur-core` / `augur-gui` and requires a system HDF5 installation. ECF-compressed Prophesee files also require the HDF5 ECF plugin via `HDF5_PLUGIN_PATH`; see [HDF5 File Support](./hdf5-file-support.md).

## GUI Workflow

1. Click `Open Replay` from the toolbar while the app is idle.
2. AugurRS opens the selected file with `RawFileCamera::open` or `DecodedEventFileCamera::open` and starts a preview-only pipeline.
3. The center panel switches to `Replay` mode and shows:
   - `Play` / `Pause`
   - `Restart`
   - replay speed selection (`0.25x` through `Max`)
   - a timeline slider for seeking
   - current / total replay time
   - MB progress as secondary info
   - keyboard shortcuts: `Space` toggles play/pause, while `←` / `→` pause replay if needed and step one acquisition-window frame backward / forward from the currently displayed replay frame
4. Enabled analysis plugins continue to run on replayed frames exactly as they do on live preview frames.

If a `<capture>.toml` sidecar exists next to the selected replay file, the replay session uses it as read-only reference data for both the left camera settings panel and the top-bar global `Settings` menu.
If the sidecar is missing, raw EVT3 replay still reuses any recorded `pixel_pitch_nm` header field so the default replay config and scale-bar math stay closer to the original capture.

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
- `.raw` offsets are aligned to EVT3 word boundaries relative to the data start
- decoded-file offsets are aligned to packed 14-byte event boundaries
- the decoder naturally resynchronizes from nearby time words after a `.raw` reopen; decoded replays reuse the cached event vector directly

This is intentionally lightweight and fast rather than perfectly timestamp-aware.

Raw replay re-establishes its pacing baseline after a seek on the first decoded packet timestamp.
Decoded replay can reset immediately from the reopened event index because the event vector is
already in memory.

On the GUI side, the transport reflects the requested seek position immediately, then snaps to the
actual decoded frame position once the replacement frame arrives. The previous rendered frame stays
visible during that handoff instead of dropping to the empty replay placeholder.

Arrow-key single-frame stepping now uses the displayed replay timestamp as the source of truth
instead of routing through the timeline fraction. The GUI reopens replay from the start of the
target acquisition window so `←` / `→` land on the previous or next frame much more
deterministically, while timeline dragging keeps the existing lightweight fraction-based seek path.

## Files

| File | Role |
|---|---|
| `augur-core/src/replay.rs` | `RawFileCamera`, `ReplayFileInfo`, fast metadata scan, reopen-at-offset support for `.raw` |
| `augur-core/src/decoded_replay.rs` | `DecodedEventFileCamera`, `PackedEventPreviewDecoder`, decoded `.csv` / `.bin` / `.npy` / optional `.h5` parsers |
| `augur-core/src/error.rs` | `CameraError::Eof` |
| `augur-core/src/pipeline.rs` | preview-frame queue drops, EOF finalization, preview-queue drain on shutdown |
| `augur-gui/src/app.rs` | replay mode, persisted EOF state, transport controls, theme-aware viewport visuals, decoded-event seek cache, restart, replay display cadence |
| `augur-gui/src/viewer_widget.rs` | shared replay transport UI, phosphor icon controls, and `Space` / arrow-key shortcuts |

## File Compatibility

Some `.raw` files may differ from the standard header convention in three ways:

1. **Non-UTF-8 header bytes** — the header parser uses `read_until` + `from_utf8_lossy` so stray binary bytes are tolerated.
2. **Missing `% format` line** — if only `% geometry WxH` is present, the parser proceeds and assumes EVT3. A `% format` line that declares a non-EVT3 codec is still rejected.
3. **Single trailing padding byte** — the published `evt3-core` crate now exposes `finish_stream_lenient()`, so replay EOF can discard one benign trailing byte while the preview pipeline reports genuine finalization errors again.

## Verification

- `cargo fmt --all`
- `cargo build --workspace`
- `cargo build -p augur-gui --features hdf5`
- `cargo test --workspace`
- `cargo test -p augur-core --features hdf5`
