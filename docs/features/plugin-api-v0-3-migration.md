# Plugin API v0.3 Migration

## Who Needs This

Any runtime plugin built against an older `augur-plugin-api` must rebuild before loading into this
version of `augur-gui`.

The host still rejects stale plugins through the vtable-size check. There is no compatibility shim.

## EventStore Changes

The old contiguous-slice accessors are gone.

Replace:

- `all_events()`
- `events_in_range(start, end)`

With:

- `frame(index)`
- `frames()`
- `frames_in_range(start, end)`
- `collect_events_in_range(start, end, out)`

The host now retains event history as complete frame windows, not one memmoved flat vector.

## Host View Changes

Plugins can now report per-dataset generations:

```rust
fn host_view_dataset_generation(&self, dataset_id: &str) -> u64 {
    0
}
```

Use `0` only for datasets that are truly static for the lifetime of the plugin instance. Dynamic
datasets should increment their generation whenever the serialized dataset bytes would change.

## Context Publishing Changes

JSON helpers still exist:

- `publish`
- `publish_persistent`

New raw helpers are available for high-frequency payloads:

- `publish_raw`
- `publish_persistent_raw`

Use the raw helpers when the plugin already owns a stable serialized representation and wants to
avoid repeated `serde_json::to_vec` calls.

## Not Included Here

This repository documents the ABI change but does not update sibling repositories such as
`augur-plugins` in this task. Those plugins must be rebuilt and adapted separately.
