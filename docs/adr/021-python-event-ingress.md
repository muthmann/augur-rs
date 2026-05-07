# ADR 021: Python Event Ingress Through Packed Preview Pipeline

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

Augur already has a decoded replay path built around `PacketStreamCamera`,
`PackedEventPreviewDecoder`, and `LiveEventSource`. Feeding Python-published
events through that path lets the first feature reuse the proven accumulation,
preview-frame, viewer-tool, 3D, and plugin behavior.

## Decision

Add a GUI-owned Python ingress listener in `augur-gui`.

- The listener binds to `127.0.0.1:57295` by default.
- The protocol is versioned JSON control messages plus binary
  `packed_xypt_v1` event batches.
- Python sends bounded chunks; Augur acknowledges each batch after it is queued.
- Augur adapts the incoming packed chunks to `PacketStreamCamera`.
- The GUI starts `spawn_pipeline` with `PackedEventPreviewDecoder`.
- Published data is treated as a finite external preview stream in this first
  cut, not as a seekable replay file.

## Consequences

### Positive

- Users can send NumPy event arrays into Augur with one Python call.
- The first implementation has predictable memory behavior: one chunk-sized
  packed allocation at a time plus a bounded queue.
- Existing Augur viewer, 3D, and plugin code paths remain unchanged.
- The protocol is independent of evt3's internal Python container
  implementation.
- The loopback-only server avoids accidental remote control surfaces.

### Negative

- This is not cross-process zero-copy.
- Python-published streams are not seekable replays in this stage.
- Columnar NumPy arrays are packed before Augur decodes them again into its
  internal frame/event source.
- Starting a new Python stream while another live camera/replay/recording
  session is active is rejected.

### Neutral

- The later external `EventSource` design can replace the packed transport
  under the same Python-facing API.
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

