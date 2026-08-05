# ADR 020: Unified Upstream Event Source

## Status

Proposed

Supersedes ADR 007 and ADR 008 once accepted.

Implementation note: the shared event-types crate, live upstream
`LiveEventSource`, GUI point-cloud projection, decoded replay `EventSource`,
cold-scan raw replay `EventSource`, and GUI-hosted plugin history fed by a
dedicated lossless upstream cursor have landed. This ADR remains Proposed until
recording-file sources and indexed raw replay repagination are complete.

## Context

Before this migration, the investigation dataflow had multiple raw-event
histories:

- `PreviewFrame.events` is carried through the bounded preview-frame channel
- `PointCloudState` stores a GUI-owned history for the 3D raw-event cloud
- `EventStore` stores copied FFI frame segments for runtime plugins

The bounded preview channel is intentionally lossy. That is acceptable for UI
projections, but it is not acceptable for recording or plugin analysis that
must see every decoded event or fail loudly.

At the same time, `augur-core` and `augur-plugin-api` must remain independent.
Sharing event-history machinery through either crate would create the wrong
dependency direction and make plugin builds inherit camera/runtime concerns.

## Decision

Introduce `augur-event-types` as a small shared crate for the raw-event
timeline surface:

- `CompactEvent` is the shared 16-byte event representation used at the plugin
  boundary and by the planned upstream ring.
- `FrameWindowEntry` stores logical event indices, timestamp windows, explicit
  physical offsets, and generations because skip-on-straddle padding is not
  part of the logical event index.
- `EventRing` is a single-writer, multi-reader in-memory ring that preserves
  per-frame physical contiguity. If a frame would straddle the physical end,
  the writer skips the tail and starts the frame at physical zero.
- `EventSource` is the common read interface for live capture, replay, decoded
  replay, recording files, and future overflow sources.
- `ConsumerCursor` and `CursorPolicy` model lossless consumers separately from
  best-effort projections.

The target architecture is:

- decode writes raw events into one upstream `EventSource`
- recording and runtime plugins consume through lossless cursors
- GUI 2D/3D/replay surfaces remain lossy projections with small derived caches
- host-view datasets remain in `host_views.rs` and are not folded into the raw
  event ring

The implementation has landed incrementally: the shared crate and plugin ABI
type move, live decode-thread wiring, GUI projections, dedicated plugin-history
cursor, decoded replay source, and cold-scan raw replay source are in place.
Indexed raw replay / recording-file repagination remains the open cutover item.

## Consequences

### Positive

- `augur-core`, `augur-gui`, and `augur-plugin-api` can depend on shared raw
  event contracts without depending on each other.
- Plugin analysis has a path to lossless reads that is upstream of preview
  queue drops.
- Per-frame contiguity keeps `FfiEventFrame` as one `ptr+len` slice.
- Padding is explicit in the frame index, so logical event indices count only
  real events.

### Negative

- The plugin ABI changes because `FfiCdEvent` now aliases `CompactEvent`; stale
  plugins must rebuild and are rejected by the ABI version guard.
- The ring introduces a published capacity-sizing concern. A frame larger than
  the ring capacity, or a protected frame that cannot be evicted, is a
  correctness error rather than a silent truncation point.
- Raw replay indexing requires follow-up work before the architecture is fully
  realized. Until then, `.raw` `EventSource` reads are correct but cold-scan the
  file, while the existing streaming/seek path still handles transport replay.

### Neutral

- Disk spool remains an overflow/replay `EventSource`, not the primary live
  storage path.
- UI preview drops remain valid for rendering surfaces, but they are no longer
  allowed to define plugin or recording fidelity. Runtime plugin history is
  copied from a dedicated upstream lossless cursor; if that cursor falls behind,
  the host surfaces an error instead of silently continuing with missing
  history.
- `BackpressureBehavior` is currently an advisory policy stored on lossless
  cursors. The ring reports `ConsumerFellBehind`; the caller decides whether to
  halt, retry, or use an overflow source. Dispatching `BlockWriter` /
  `SpillToOverflowSource` inside the ring is deferred until the writer has an
  explicit clock and callback surface.

## References

- [Investigation Dataflow And Memory Model](../features/investigation-dataflow-and-memory-model.md)
- ADR 007: Host-Owned EventStore for Plugin History
- ADR 008: Segmented EventStore and Generation-Aware Host View Caching
