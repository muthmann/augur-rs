# ADR 030: Reader-Owned Transfer Buffers And Recording Completeness Accounting

## Status

Accepted

## Context

Recordings are the primary product of this tool, and a gap in a `.raw` file is
unrecoverable: the events that were dropped never existed on the host. Two
earlier rounds of work addressed known host-side causes — ADR 023 moved camera
control off the stream thread so a reconfigure cannot pause reads, and the
lossless-disk-path work raised the reader↔writer budget to 16 MiB so a slow disk
write does not immediately starve the reader. Recordings were still coming back
with gaps.

Three structural problems remained.

**1. The transfer queue drained whenever the pipeline could not accept data.**
`AsyncBulkStreamReader` keeps eight bulk IN transfers queued in the kernel, and
it only reaps and re-arms them from inside `read_packet`. The stream loop,
however, waited for a free raw buffer *before* calling `read_packet`. Once the
buffer pool ran empty — the exact situation a disk hiccup produces — the loop
parked on the pool and stopped servicing libusb entirely. The queued transfers
completed and were never re-armed, so after ~512 KiB the device endpoint had no
transfer to write into at all and the camera FIFO overflowed. A short downstream
hiccup therefore produced a gap far longer than the hiccup itself. Re-arming was
also coupled to delivery: a transfer could only go back on the endpoint after
its data had been copied into a pipeline buffer.

**2. Data the host already held was thrown away at stop.** On stop the loop
broke immediately. Every transfer that had already completed — up to eight
64 KiB buffers of events the camera had successfully delivered — was discarded
with the reader. Partial data returned by timed-out and cancelled transfers was
discarded on every reap for the same reason.

**3. Loss was unmeasurable.** The transport exposes no device-side overflow
counter, and the pipeline recorded nothing about whether the recording path had
ever failed to accept data. A researcher could not tell a complete recording
from a gapped one, either while recording or afterwards from the file.

A fourth, different problem sits one layer up: live plugin analysis is
best-effort by design (ADR 025). The GUI drains the preview channel to the
newest frame and dispatches only that one frame to the plugins, so a live series
is sampled from a fraction of the accumulation windows. That is intentional, but
it was invisible, and a sparsely sampled live series looks exactly like a
complete one.

## Decision

**The reader owns its buffers and can be serviced independently of delivery.**
`AsyncBulkStreamReader` holds a pool of spare buffers alongside the transfer
slots. A completed transfer is re-armed with a spare immediately and its filled
buffer is queued for delivery, so re-arming never waits for the pipeline.
Timed-out and cancelled transfers keep whatever bytes the device already sent.

**`PacketStreamReader` gains two methods, both defaulted.**

- `service(budget)` — reap completions and re-arm transfers without delivering
  a packet. The stream loop calls it while it has no buffer to read into, so the
  endpoint stays fed through a downstream stall.
- `take_buffered_packet(out)` — hand over one already-received packet. The
  stream loop drains it into the disk queue after stopping, so the tail of a
  recording contains everything the host had.

Both default to a no-op, which is correct for synchronous readers that hold
nothing queued.

**Buffer-pool starvation is a first-class, recorded event.** `acquire_raw_buffer`
records every wait for a free raw buffer: count, total, and maximum duration.
Those numbers are the honest statement of recording integrity — the transport
buffers a bounded amount, so a long stall means the camera FIFO overflowed.

**The file states its own completeness.** `RecordingMetadata` carries
`reader_stall_events`, `reader_stall_max_us` and `reader_stall_total_us`,
written to the sidecar at shutdown. `Some(0)` is the positive claim "recorded
without a single stall"; `None` marks a recording written before this existed.
This extends the self-describing-metadata contract of ADR 013.

**Live analysis coverage is quantified, not just labelled.** The GUI counts
accumulation windows delivered by the pipeline and windows actually dispatched
to plugins, and states the ratio in the live-analysis notice.

## Consequences

- A downstream stall now costs roughly the stall itself rather than the stall
  plus a dry endpoint, and stopping a recording no longer truncates it by the
  contents of the transfer queue.
- Reader memory grows by one spare buffer per transfer (8 × 64 KiB = 512 KiB).
- Loss is bounded and visible instead of silent, in the GUI diagnostics, in
  `augur record`'s exit summary, and in the recording's own sidecar.
- The stall counter is a host-side measure. It cannot see a device-side FIFO
  overflow caused by something other than host backpressure; surfacing
  device-side counters still depends on the transport exposing them.
- Live analysis remains best-effort (ADR 025). This ADR does not change the
  live path's sampling — it makes the sampling rate legible so live series are
  never mistaken for complete measurements. Complete, gap-free results continue
  to come from analysis runs.
