# ADR 023: Split Control And Stream Transport Threads

## Status

Accepted

## Context

ADR 002 established the three-thread streaming pipeline (USB reader, disk
writer, preview). Camera control — initial configure, start/stop streaming,
and runtime reconfiguration — ran inline on the USB reader thread between
packet reads. USB control transfers can take tens of milliseconds (a full
reconfigure issues many register writes with 1 s timeouts), and while they
run, no stream reads happen. A paused reader lets the camera-side FIFO
overflow, which produces unrecoverable multi-millisecond event gaps in the
recording. Lossless recording is the project's top priority, so applying
settings during a recording must not pause stream reads.

libusb explicitly supports concurrent synchronous transfers on different
endpoints from different threads, so control transfers and stream bulk reads
can safely overlap on one shared device handle.

## Decision

- `augur-prophesee::Transport` shares its `DeviceHandle` behind an `Arc` and
  is `Clone`. Control-command sequencing (Treuzell write/read pairs) stays
  single-threaded because `Treuzell` takes `&mut Transport` on one clone.
- `augur-core` adds a `PacketStreamReader` trait and an optional
  `PacketStreamCamera::split_stream_reader()` (default `None`). The EVK4
  camera returns a reader that clones the transport and reads only the
  stream endpoint.
- When a camera supports splitting, `spawn_pipeline` runs a dedicated
  control thread that owns the camera (initial configure, start/stop
  streaming, runtime reconfiguration from `settings_tx`), while the stream
  thread reads packets exclusively. Cameras without split support (replay,
  tests, in-memory sources) keep the previous inline behavior via an
  `InlineCameraWorker`; both paths share one `run_stream_loop`.
- The GUI asks for explicit confirmation before applying settings during an
  active recording, because a reconfigure is still visible in the recorded
  data (bias/ROI/filter changes take effect immediately in the sensor).

## Consequences

- Applying settings during recording no longer pauses stream reads; the
  recording stays gap-free through a reconfigure.
- The pipeline has up to four threads (control, stream, disk, preview); all
  are joined by `PipelineController::shutdown`.
- Sensor-side effects of a reconfigure (changed sensitivity, brief settling
  disturbance) are inherent and are surfaced to the user via the GUI
  confirmation dialog rather than hidden.
- A future async multi-URB read path can slot in behind `PacketStreamReader`
  without touching pipeline threading again.
