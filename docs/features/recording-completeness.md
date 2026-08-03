# Recording completeness

Recordings are unrecoverable if they have gaps: the events that were never read
off the camera do not exist anywhere. This brief describes what the capture path
does to avoid gaps, and — where a gap is unavoidable — how the system reports it
instead of hiding it.

Related: [ADR 030](../adr/030-recording-completeness-accounting.md),
[ADR 023](../adr/023-split-control-and-stream-transport.md),
[ADR 013](../adr/013-self-describing-recording-metadata.md),
[ADR 025](../adr/025-analysis-runs-primary-interface.md),
[Performance safeguards](./performance-safeguards.md).

## The capture path

```
camera ──USB bulk IN──▶ AsyncBulkStreamReader ──▶ stream loop ──▶ disk queue ──▶ writer
          (8 queued          (+8 spare               (raw buffer     (256 ×
           transfers)         buffers)                pool)           64 KiB)
```

Everything left of the disk queue is lossless by contract. The preview branch
that hangs off the stream loop is explicitly best-effort and may drop packets;
that never affects the recording.

### The transfer queue never runs dry on a downstream stall

The reader keeps eight bulk IN transfers queued in the kernel so there is no
dead time between reads. It owns a spare buffer per transfer, so a completed
transfer is re-armed *immediately* and its filled buffer is queued for delivery
separately — re-arming never waits for the pipeline to hand a buffer back.

While the raw-buffer pool is empty the stream loop calls
`PacketStreamReader::service`, which reaps completions and re-arms transfers
without delivering a packet. Without this the loop would park on the pool and
stop servicing libusb altogether, the endpoint would run dry, and a short disk
hiccup would turn into a much longer camera-FIFO overflow.

Timed-out and cancelled transfers keep the bytes the device already sent.

### Stopping does not truncate

At stop the reader usually still holds transfers that completed but were never
delivered. `PacketStreamReader::take_buffered_packet` hands those over, and the
stream loop drains them into the disk queue before the reader is torn down. The
disk thread already drains until the stream thread drops its sender, so nothing
accepted by the reader is lost to the stop flag.

The drain is bounded (500 ms, 64 packets). The camera is not guaranteed to have
stopped streaming yet — the control thread stops it on its own tick — so a
reader that keeps re-arming transfers could otherwise keep producing packets
forever. The drain flushes what the host already held; it does not keep
recording.

Regression tests: `packets_already_received_by_the_reader_reach_the_recording_after_stop`
and `disk_writer_persists_packet_accepted_after_stop_request`
(`augur-core/src/pipeline.rs`).

## What is measured

The raw-buffer pool is the pipeline's backpressure. While it is empty the
recording path cannot accept data, and the transport only buffers a bounded
amount — so a long wait means the camera FIFO overflowed and those events are
gone. Every wait is recorded:

| `PipelineStatsSnapshot` field | Meaning |
| --- | --- |
| `raw_pool_starvation_events` | How often the recording path could not accept the next packet |
| `raw_pool_starvation_max_us` | Longest single stall — the headline integrity number |
| `raw_pool_starvation_us` | Total time unable to accept data |
| `stop_drain_packets` | Packets recovered from the reader at stop |

`PipelineStatsSnapshot::recording_may_have_gaps()` is the single question a
caller should ask.

## Where it is reported

- **GUI** — the viewer's Diagnostics section states either
  "Recording path: no stalls, no host-side event loss." or a highlighted
  "Recording path stalled N× (longest … ms, total … ms) — the recording may have
  event gaps."
- **CLI** — `augur record` prints the same verdict when it stops.
- **The recording itself** — the `.toml` sidecar carries `reader_stall_events`,
  `reader_stall_max_us`, and `reader_stall_total_us`. `reader_stall_events = 0`
  is a positive claim that the file was recorded without a single stall; absent
  fields mean the recording predates this accounting.

## Live analysis is sampled, and says so

Live plugin output is best-effort by design (ADR 025): the GUI drains the
preview channel to the newest frame and dispatches only that frame to the
plugins, so a live series contains one sample per *processed* window, not per
accumulation window. That is a deliberate trade for GUI responsiveness, but a
sparsely sampled series looks exactly like a complete one.

The live-analysis notice therefore quantifies it, e.g. "Live plugin output is a
preview covering 1 of 16 accumulation windows (6 %), so live series have gaps —
start an analysis (Analysis ▸ New Analysis…) for exact, gap-free results."

**Complete measurement series come from analysis runs**, which tile the whole
recording into fixed windows and process every one of them
(`augur-runtime/src/offline.rs`). Live output is for watching, not for
measuring.

## The ImageJ bridge is sampled too, and now says so

The same rule applies to preview frames handed to an external tool. The ImageJ
bridge holds a bounded queue (32 envelopes) and drops rather than back-pressure
the preview path — correct, since it is a preview, not a capture path — but it
used to report `Streaming` regardless, so an incomplete series arriving in
ImageJ looked complete.

`ExternalTool::throughput()` now returns `frames_offered` and `frames_dropped`.
Any drop turns the top-bar chip to warning tone (`ImageJ: Streaming · 37
dropped`) and adds a delivered/dropped breakdown to the `Stream to ImageJ`
dialog, pointing at TIFF export or an analysis run for a gap-free series
(ADR 033).

## Known limits

- The stall counter is host-side. It cannot observe a device-side FIFO overflow
  with another cause; device-side overflow counters need transport support that
  does not exist yet.
- If the sustained disk write rate is below the camera's event rate, the 16 MiB
  in-flight budget only delays the loss. The system will report it, but it
  cannot prevent it.
