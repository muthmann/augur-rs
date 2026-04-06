# ADR 014: Timestamp-Driven Replay Pacing

## Status
Accepted

## Context

A global byte-rate estimate for replay pacing causes `1x` speed to drift away from recorded event time whenever event density varies across the file: dense regions replay too slowly and sparse regions replay too quickly. Replay display cadence must also adapt to replay speed and acquisition time to avoid wasting repaints or silently dropping frames.

## Decision

Adopt a timestamp-driven replay timing model:

1. Replay backends pace against event timestamp deltas instead of bytes read.
2. `ReplayControls` carries a shared `current_timestamp_us` atomic and raw
   EVT3 replay updates it directly from the packet reader, so pacing is not
   coupled to the lossy preview queue.
3. Replay speed changes and seeks reset their pacing baseline from the current
   timestamp rather than consumed byte/event count.
4. `augur-gui` derives the effective replay display interval from
   `acq_time_ms / speed`, clamped to `10..=200` ms, while leaving the manual
   preview-rate control as a live-mode preference.
5. The preview thread drops a replay frame before constructing `PreviewFrame`
   when the downstream frame queue is already full.

## Consequences

- `1x`, `2x`, and other finite replay speeds now track recorded event time much
  more closely, even for files with uneven event density.
- Raw replay still uses a one-packet timing lag, but that lag now comes from
  its own packet-reader state instead of preview-thread feedback.
- Replay no longer treats the user's manual preview-rate setting as authoritative
  for the 2D replay surface; the GUI instead shows the derived effective rate.
- High-speed replay intentionally sacrifices some preview frames when the GUI is
  behind in order to keep the pipeline cheaper and more predictable.
