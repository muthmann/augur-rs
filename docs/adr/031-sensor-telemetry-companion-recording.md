# ADR 031: Optional Sensor-Telemetry Companion Recording

## Status

Accepted

## Context

ADR 029 introduced live IMX636 monitoring, but its cached GUI/plugin snapshot
had only a host-side age and was not durable. Copying that snapshot per preview
frame would duplicate stale values, inherit preview drops, and falsely imply
frame synchrony.

EVT3 event/trigger timestamps are relative IMX636 sensor time with a nominal
1-µs tick. The dead-time, LIFO, temperature, and bias register reads return no
sensor timestamp and execute sequentially. USB/firmware buffering between
sensor time and host receipt is not publicly bounded. There is therefore no
defensible exact conversion from a monitoring sample to an event timestamp.

The RAW event stream is the primary measurement. Monitoring must never add
backpressure to it, and the feature must remain an explicit opt-in.

## Decision

- Add an optional `SensorTelemetryOptions` to `PipelineOptions`. The GUI option
  defaults to off and is immutable during recording.
- Write `<raw-stem>.sensor-monitoring.csv` through a dedicated bounded queue and
  `BufWriter`. The camera-control thread submits with `try_send`; full or failed
  telemetry drops telemetry, never RAW.
- Reject enabled telemetry when the companion file cannot be created or the
  camera lacks an independent control thread.
- Add the backwards-compatible
  `EventCamera::read_monitoring_selected(SensorMonitoringSelection)` method.
  Its default filters a monolithic read; IMX636 overrides it with actual
  selective register access.
- Poll lux at 1.5 Hz, temperature/dead time at 0.2 Hz, and biases at start or
  after reconfiguration. Do not catch up missed deadlines.
- Timestamp every physical poll with a host-monotonic start/end interval and
  bracket it with cumulative RAW payload byte offsets before/after the poll.
  Do not publish a fabricated `event_timestamp_us` or numeric uncertainty.
- Store the companion filename, cadence, clock semantics, row/drop/error counts
  in the normal recording sidecar.
- Keep sensor values and camera lifecycle actions together in a visible Capture
  card, with a central idle-state call-to-action. Menus remain secondary.

## Consequences

- Offline analysis can map byteoffset brackets into the decoded RAW timeline
  without depending on lossy preview frames. The association is auditable but
  still subject to unknown transport/register latency.
- Exact timing remains available only through an event-clock source such as
  `EXT_TRIGGER`; sensor monitoring remains contextual telemetry.
- Different physical read rates reduce control traffic and avoid repeatedly
  waking the temperature/dead-time blocks at the lux rate.
- CSV stalls and failures cannot stall the stream reader. Dropped telemetry is
  explicit in controller state and the sidecar.
- A hardware A/B test is still required before claiming that 1.5-Hz monitoring
  has zero influence at maximum event/link load.

## Alternatives considered

- **Log the GUI/plugin snapshot.** Rejected because it repeats cached values,
  follows a best-effort preview lifecycle, and lacks acquisition boundaries.
- **Decode every RAW packet on the stream thread to fabricate a current event
  timestamp.** Rejected because the extra hot-path work conflicts with event
  priority and still cannot remove USB/firmware latency.
- **Write CSV synchronously from the control thread.** Rejected because storage
  latency would delay monitoring and settings handling.
- **Embed monitoring into EVT3.** Rejected because it is not an EVT3 sensor
  event, has a different clock/provenance, and would reduce file compatibility.

## References

- ADR 023 — split stream/control transport threads
- ADR 029 — monitoring register provenance and trust boundary
- ADR 030 — RAW recording completeness and stall accounting
- `docs/features/sensor-telemetry-recording.md`
- `docs/features/absolute-setting-values.md`
