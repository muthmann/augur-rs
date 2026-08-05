# ADR 021: Python Event Ingress Through In-Memory Replay Pipeline

## Status

Accepted

## Context

The companion evt3 Python package can now expose decoded events as stable NumPy
arrays and publish those arrays through a small `evt3.augur` connector. Augur
needs to receive that data in a way that is easy for users, bounded in memory,
and compatible with the existing viewer, 3D, and plugin pipeline.

The long-term target is an external event timeline or `EventSource` that can
borrow shared-memory or memory-mapped Python data directly. That is not the
lowest-risk first step because it introduces cross-process lifetime,
synchronization, and columnar-versus-packed layout decisions.

Augur already has a decoded replay path built around `DecodedEventFileCamera`,
`PackedEventPreviewDecoder`, replay controls, and `LiveEventSource`. Opening
Python-published events through that path lets the feature reuse the proven
accumulation, preview-frame, viewer-tool, 3D, plugin, pause/resume, step, and
seek behavior.

## Decision

Add a GUI-owned Python ingress listener in `augur-gui`.

- The listener binds to `127.0.0.1:57295` by default.
- The protocol is versioned JSON control messages plus binary
  `packed_xypt_v1` event batches.
- Python sends bounded chunks; Augur acknowledges each batch after decoding it
  into a `Vec<CdEvent>`.
- After `finish_events`, Augur opens the completed dataset as an in-memory
  decoded replay with `DecodedEventFileCamera`.
- The GUI starts `spawn_pipeline` with `PackedEventPreviewDecoder` in replay
  mode, decodes the first frame, and pauses for inspection.
- Published data is treated as a finite, seekable in-memory replay.

## Consequences

### Positive

- Users can send NumPy event arrays into Augur with one Python call.
- Researchers get the same replay controls for Python data as file replay:
  play, pause, seek, step, restart, 2D inspection, 3D inspection, and plugin
  analysis.
- Existing Augur viewer, 3D, and plugin code paths remain unchanged.
- The protocol is independent of evt3's internal Python container
  implementation.
- The loopback-only server avoids accidental remote control surfaces.

### Negative

- This is not cross-process zero-copy.
- Columnar NumPy arrays are packed before Augur decodes them again into its
  internal replay timeline.
- Augur holds the completed Python dataset in memory so replay controls can
  seek and step without asking Python to resend data.
- Starting a new Python stream while a live camera preview, recording, or
  non-Python replay is active is rejected.

### Neutral

- The later external `EventSource` design can replace the in-memory decoded
  timeline under the same Python-facing API.
- The protocol validates transport metadata, while evt3 remains responsible
  for NumPy dtype, shape, timestamp ordering, and geometry validation before
  publication.

## Alternatives Considered

### Shared Memory First

Rejected for the first cut. It is the right direction for later zero-copy work,
but it requires explicit lifetime, cleanup, mutation, and layout contracts
before the user-facing workflow is proven.

### Arrow IPC

Rejected for this path. Arrow is useful for columnar tables, but Augur's
current hot path already has a compact event timeline and packed decoded-event
transport. Arrow would add another data model without removing the need for an
Augur control protocol.

### File-Based `.npy` Replay

Already supported, but it forces users to write intermediate files and breaks
the interactive Python analysis loop this feature is meant to improve.
