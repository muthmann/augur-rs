# Replay

## Summary

AugurRS can replay recorded EVT3 `.raw` files plus decoded `.csv`, `.bin`, `.npy`, and optional `.h5` / `.hdf5` event files through the same preview and plugin pipeline used for live camera sessions. Replay keeps the last frame visible at EOF, supports restart and seek operations without leaving replay mode, restores recorded raw-file metadata when present, accepts Prophesee-style RAW headers that omit `% end` or `% geometry` when the remaining metadata is still enough to infer the stream, resets its pacing baseline cleanly when speed changes, unwraps raw EVT3 timestamps across the 24-bit sensor rollover so replay time stays monotonic on long captures and later seeks, supports keyboard play/pause and frame stepping, keeps paused forward frame steps on the active replay controller instead of resetting byte progress through a reopen, reuses a small cache of already displayed replay frames for immediate backward and return-forward stepping, and finalizes older `.raw` files without suppressing genuine preview errors.

## Core Design

Replay lives in `augur-core` as two file-backed camera adapters, so the rest of the stack can treat file playback like any other packet-stream camera:

- `RawFileCamera` parses the EVT3 header (`% format`, `% geometry`, `% evt`, metadata lines, optional `% end`), tolerates older files that omit `% format` or contain non-UTF-8 header bytes, can end the header at the first non-`% ` payload line, falls back to Prophesee plugin metadata or a first-payload event scan when geometry is missing, scans only the first and last replay windows to estimate duration/byte rate, and reopens at arbitrary EVT3-aligned offsets through `RawFileCamera::open_at`
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
- raw EVT3 replay unwraps the packet-reader and preview-frame timestamps across the 24-bit EVT3 rollover instead of letting transport time jump back toward zero
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
   - keyboard shortcuts: `Space` toggles play/pause, while `←` / `→` pause replay if needed and step one acquisition-window frame backward / forward
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

Decoded replay files also expose that shared event vector through
`DecodedReplayEventSource`, a timestamp-range `EventSource` used by the
upstream raw-event migration. Raw `.raw` replay exposes `RawReplayEventSource`
with correct cold-scan range reads. The remaining ADR 020 performance work is a
sparse `.idx` sidecar keyed by file size, mtime, and header bytes, with
timestamp / byte-offset checkpoints that let the host seek before a requested
timestamp, decode forward, and repaginate events without scanning from the data
start each time.

For raw `.raw` seeks, both the packet-reader timing feedback and the preview decoder are seeded
from the reopened byte position so the first replacement frame lands in the correct EVT3 rollover
epoch instead of appearing near timestamp `0`.

On the GUI side, the transport reflects the requested seek position immediately, then snaps to the
actual decoded frame position once the replacement frame arrives. The previous rendered frame stays
visible during that handoff instead of dropping to the empty replay placeholder.

For paused raw replay seeks and `←` / `→` frame steps, Augur keeps decoding after the reopen until
the requested target timestamp is actually reached; it does not stop on the first decoded frame if
that frame still lands earlier than the requested acquisition window.

Whenever replay opens or reopens the preview pipeline, it also reapplies the current replay
acquisition-time setting to the new controller, so seek/restart/step paths keep using the same
frame-window length instead of drifting back to the default `50 ms` preview window.

Replay preview accumulation also now closes frames at the first decoded event that reaches the
requested acquisition window, even when one file-read / decode packet spans multiple frame windows.
That keeps replay event counts and visible frame content responsive to acquisition-time changes
instead of letting large packet boundaries dominate the frame size.

Paused `→` frame steps reuse the current replay controller whenever it is still active. Instead of
reopening the file from an approximate byte offset, Augur briefly resumes decoding from the current
position until the requested next acquisition window is reached, so the MB progress keeps advancing
from the current byte position instead of jumping back.

Paused `←` frame steps first walk a small in-memory history of already displayed replay frames. As
long as the requested previous frame is still inside that retained window, Augur restores it
directly, including its displayed byte progress, without reopening the file. If the user resumes
playback from one of those rewound snapshots, replay reopens from the displayed position so
playback continues from what is on screen instead of jumping back to the newer paused controller
state.

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

1. **Non-UTF-8 header bytes** — the header parser uses lossy text conversion for header lines so stray non-UTF-8 bytes do not immediately reject older files.
2. **Missing `% end` terminator** — Augur follows Prophesee's RAW parsing rule that `% end` is optional and the payload begins as soon as the next line no longer starts with `% `.
3. **Missing `% format` line** — if only `% geometry WxH` is present, the parser proceeds and assumes EVT3. A `% format` line that declares a non-EVT3 codec is still rejected.
4. **Missing geometry** — Augur first tries known Prophesee plugin-family fallbacks such as `hal_plugin_gen41_*`, `hal_plugin_imx636_*`, `hal_plugin_gen31_*`, and `hal_plugin_genx320_*`; if that still fails, it decodes an initial payload window and derives a best-effort geometry from the observed event bounds.
5. **Single trailing padding byte** — the published `evt3-core` crate now exposes `finish_stream_lenient()`, so replay EOF can discard one benign trailing byte while the preview pipeline reports genuine finalization errors again.

## Verification

- `cargo fmt --all`
- `cargo build --workspace`
- `cargo build -p augur-gui --features hdf5`
- `cargo test --workspace`
- `cargo test -p augur-core --features hdf5`
