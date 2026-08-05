# ADR 024: Host/Worker Plugin State Ownership

## Status

Accepted.

## Context

The live-worker cutover (ADR 022) left three pieces of plugin-runtime state
with ambiguous owners, and each ambiguity produced a real defect:

1. **Retained-history cursor.** The pipeline registers a lossless
   `plugin-runtime` cursor at spawn for the GUI scrub executor, but the GUI
   only drained it on the paused path. While the live worker was active the
   cursor never advanced, so once the event ring wrapped, every archival
   append failed (`ConsumerFellBehind`) and retained plugin history plus 3D
   raw views silently starved. The GUI event store also unregistered that
   controller-owned cursor on detach, leaving a stale id behind.
2. **Persistent context bus.** Every job echoed the GUI's whole persistent
   map and the worker `extend`ed it over its own newer state, rolling back
   plugin-published values whenever the worker lagged one UI tick. There was
   also no way to clear the worker's map, so `reset_analysis` was undone by
   the next result and cross-source values resurrected.
3. **Action request queue.** `pending_action_requests` was never retired:
   unbounded growth (including cluster-row snapshots), re-serialization every
   frame, and full-queue replay onto rebuilt plugin instances.

## Decision

Make ownership explicit:

- **Cursor.** The controller owns the `plugin-runtime` cursor; the GUI drains
  it on every dequeued frame batch regardless of which executor runs plugins,
  registers it on demand when a retained-history plugin is enabled
  mid-session, and releases it when no plugin needs history.
  `PluginEventHistory` only unregisters cursors it registered itself
  (borrowed vs. owned).
- **Persistent bus.** The worker's map is authoritative between jobs. Jobs
  carry either a full seed snapshot (startup, epoch bumps, after the
  synchronous executor ran) or incremental host-authored upserts/removals.
  Coalesced jobs merge their updates. An epoch-tagged `ClearPersistent`
  command wipes the worker map on `reset_analysis`.
- **Action queue.** Requests are delivered at-least-once to the executor
  active at apply time and then retired: the live path retires up to the
  request-id watermark echoed back in each result; the synchronous path
  retires immediately after its pass; `reset_analysis` drops the queue. The
  serialized queue is only re-published when it changed.

Additionally, raw-event visibility is now identical across live, paused, and
offline paths (only `RawEvents`-phase plugins see current-frame raw events),
and `on_discontinuity` is delivered to every plugin, with state-kind
filtering left to the trait's default implementation.

## Consequences

- Retained event history stays gap-free in live mode; enabling a
  retained-history plugin mid-session gets lossless coverage from that point.
- Plugin persistent state survives worker latency; resets actually reset.
- The action queue is bounded and requests no longer re-execute on rebuilt
  plugin instances or after a source change — plugins keep deduping by
  `request_id` only for the at-least-once delivery window.
- Live results match offline results for plugins that declare their input
  kind correctly; plugins that read raw events without declaring `RawEvents`
  now see empty slices everywhere instead of sometimes.
- Host-view datasets from providers without generation counters refresh once
  per analysis pass instead of freezing (generation `0` is treated as
  "no counter"), and unchanged datasets no longer re-render every frame.
