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
queue. Raw EVT3 timestamps are unwrapped across the sensor's 24-bit rollover in
both the packet-reader feedback path and the preview decoder, including mid-file
seek reopens, so replay time and arrow-key frame stepping stay monotonic on
long captures. `DecodedEventFileCamera` reads the current event timestamp
directly from its cached event vector.

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

### Scrubbing and 2D/3D window alignment

The 2D frame shows one acquisition window `[T - acq_time, T]`; the 3D point
cloud shows its own look-back window `[T - time_window, T]`. Both share the
same right-edge timestamp `T` (the displayed frame's `window_end_us`). Seeks
and steps preserve that contract:

- **Paused seeks decode a 3D look-back.** When the 3D view is open, a paused
  scrub to `T` reopens the transport one 3D time window before `T` and sprints
  forward unthrottled. Every decoded frame is archived into the event ring, so
  after the sprint the 3D history is rebuilt from the ring and shows the full
  `[T - window, T]` span instead of a single acquisition frame. The user's
  replay speed is restored when the target frame is displayed.
- **The 2D view never overshoots the seek target.** During the sprint, the
  first frame whose window reaches the target is displayed; later frames only
  feed the 3D history. Frames that arrive after replay is paused (in-flight
  packets, sprint leftovers) are added to the 3D history but do not advance
  the paused 2D display.
- **Backward steps keep the 3D history.** Stepping back through the frame
  snapshot history re-anchors the 3D summary to the older frame's end
  timestamp without clearing retained history, so the look-back window stays
  full. Only when the retained history no longer covers the snapshot's window
  does the view fall back to that single frame.

### Acquisition time during replay

The `Settings -> Acquisition time` slider now applies to the running pipeline
immediately (it is a host-side preview parameter, not a sensor setting, so it
does not go through Apply Settings — which replay mode locks out). While
replay is paused, releasing the slider rebuilds the displayed frame at the
same position with the new window.

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
