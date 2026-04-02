# Replay Timing Model

Replay now follows recorded event time instead of average file throughput.

## Behavior

### Timestamp-driven pacing

Replay speed now maps directly onto event timestamps:

- `1x` replays one second of recorded event time in about one second of wall-clock time
- `2x` replays the same data in about half the wall-clock time
- `Max` still skips throttling entirely

`RawFileCamera` updates `ReplayControls::current_timestamp_us` directly from the
raw EVT3 packets it reads, so pacing is no longer coupled to the lossy preview
queue. `DecodedEventFileCamera` reads the current event timestamp directly from
its cached event vector.

Speed changes and seeks reset the replay pacing baseline from the current
timestamp instead of the previous byte/event position.

### Replay display cadence

The top-bar `Settings -> Advanced -> Preview update [Hz]` control remains the
live-preview cadence preference. During replay, Augur now auto-derives the
effective 2D display cadence from:

`effective_interval_ms = clamp(acq_time_ms / speed, 10 ms .. 200 ms)`

That keeps the visible preview closer to the actual frame-production cadence
and the GUI shows the derived effective rate as informational text during
replay.

### Preview-thread load shedding

If the preview-frame queue is already full when the preview thread reaches the
next frame boundary, the thread now drops that frame before constructing a
`PreviewFrame`. The per-frame drop counter still records the loss, but the
thread avoids building a frame object and reallocating/recycling image buffers
for work the GUI cannot consume anyway.

## Scope

- Recording stays unchanged.
- Live camera mode keeps the manual preview-rate control.
- Replay progress still uses byte position because it remains a good seek/progress metric.
- Plugin APIs (`EventStore`, `PreviewFrame`, host views) do not change.

## Files

| File | Role |
|---|---|
| `augur-core/src/replay.rs` | raw EVT3 timestamp-based replay pacing |
| `augur-core/src/decoded_replay.rs` | decoded-event timestamp-based replay pacing |
| `augur-core/src/pipeline.rs` | queue-aware preview-frame drops |
| `augur-gui/src/app.rs` | replay pipeline wiring, effective replay display cadence, settings UI note |

## Verification

- `cargo fmt --all`
- `cargo test -p augur-core speed_epoch_reset_restarts_throttle_baseline`
- `cargo test -p augur-core speed_epoch_reset_restarts_decoded_replay_baseline`
- `cargo check -p augur-gui`
