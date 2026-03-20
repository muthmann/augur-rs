# ADR 008: Segmented EventStore and Generation-Aware Host View Caching

## Status

Accepted

## Context

The first host-owned `EventStore` design kept one flat `Vec<FfiCdEvent>` plus frame boundaries.
That made the plugin API easy to explain, but it also created two performance problems:

- evicting old history required `drain(..)` on the front of a large event vector
- the GUI could only invalidate host-view datasets coarsely, so open density views and dataset
  snapshots were rebuilt more often than necessary

At the same time, high-frequency plugin context publishing needed a cheaper path than forced
host-side JSON serialization on every call.

## Decision

Adopt three coordinated changes:

1. Store retained event history as complete frame segments.
   - `EventStore` now owns `VecDeque<StoredFrame>`
   - each stored frame owns its own `Box<[FfiCdEvent]>`
   - memory-budget enforcement evicts whole oldest frames instead of memmoving retained events
2. Expose retained history to plugins as frame windows instead of one contiguous slice.
   - `FfiEventFrame`
   - `frame_at`
   - `frame_range_for_timestamps`
   - `EventStoreHandle::frames`, `frames_in_range`, and `collect_events_in_range`
3. Add host-view dataset generations and raw context publishing helpers.
   - plugins report `host_view_dataset_generation(dataset_id)`
   - the GUI keeps cached dataset snapshots and density render state until that generation changes
   - `HostContext` gains `publish_raw` and `publish_persistent_raw`

## Consequences

### Positive

- retained-event eviction is bounded by whole-frame removal instead of front-vector memmove
- plugins can still flatten a range when needed, but the host no longer pays contiguous-storage
  costs all the time
- host-view datasets and density textures survive across frames when the plugin output is unchanged
- plugins that already own serialized bytes can avoid repeated `serde_json::to_vec` calls

### Negative

- the runtime plugin ABI changes and all dynamic plugins must rebuild
- plugin authors now think in frame windows instead of one flat event slice
- the host-view cache contract depends on providers maintaining correct generation counters

## Alternatives Considered

### `VecDeque::make_contiguous()` on demand

Rejected because it only defers the large memmove until plugins ask for contiguous history, which
keeps the same pathological behavior under heavy raw-event workloads.

### Content hashing on the host for dataset/context invalidation

Rejected for this pass because it would add more CPU work to the host hot path. Explicit dataset
generations and raw publish helpers keep the contract simpler and cheaper.
