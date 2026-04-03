# Plugin API Migration History

This document traces the plugin API evolution from v0.2 through v0.4. Plugin authors upgrading
from any older version can read the relevant sections below.

The host rejects stale plugins through the `PluginVTable::vtable_size` check. There is no
compatibility shim — plugins must be rebuilt against the current `augur-plugin-api`.

---

## v0.2 — Flat Runtime ABI

`augur-plugin-api` now exposes one flat `PluginVTable` through the single `augur_plugin_vtable`
entry symbol. The host no longer supports legacy ABI descriptors, reconstruction-specific
compatibility hooks, or manifest fallbacks.

### What Changed

- `PluginVTable` includes host-view callbacks directly.
- `Plugin::process_frame()` receives `EventStoreHandle`, giving plugins read-only access to the
  retained decoded event history.
- `HostContext` now has both per-frame `publish()` / `get()` and persistent
  `publish_persistent()` / `get_persistent()` helpers.
- `augur-gui` requires a valid `plugin.toml` before attempting to load a runtime plugin.
- `augur-rs` now keeps only the runtime host/API infrastructure in-tree;
  scientific runtime plugins live in
  [augur-plugins](https://github.com/muthmann/augur-plugins), while
  hotpixel detection is a host-owned GUI feature.

### Operational Notes

- Event history retention is host-owned and memory-budgeted (default 100 MiB).
- The top-bar Settings menu exposes the EventStore memory budget.
- Any plugin built against the pre-v0.2 layer must be rebuilt.

---

## v0.3 — Segmented EventStore and Dataset Generations

### EventStore Changes

The old contiguous-slice accessors are gone.

Replace:

- `all_events()`
- `events_in_range(start, end)`

With:

- `frame(index)`
- `frames()`
- `frames_in_range(start, end)`
- `collect_events_in_range(start, end, out)`

The host now retains event history as complete frame windows, not one flat vector.

### Host View Changes

Plugins can now report per-dataset generations:

```rust
fn host_view_dataset_generation(&self, dataset_id: &str) -> u64 {
    0
}
```

Use `0` only for truly static datasets. Dynamic datasets should increment their generation
whenever the serialized bytes would change.

### Context Publishing Changes

JSON helpers still exist (`publish`, `publish_persistent`). New raw helpers are available for
high-frequency payloads:

- `publish_raw`
- `publish_persistent_raw`

Use the raw helpers when the plugin already owns a stable serialized representation and wants to
avoid repeated `serde_json::to_vec` calls.

---

## v0.4 — Companion Type Crates and Capabilities

### Domain Payload Changes

Localization/SMLM payloads moved out of `augur-plugin-api` into `augur-plugin-types`.

Replace imports like:

- `augur_plugin_api::Localization`
- `augur_plugin_api::LocalizationResults`
- `augur_plugin_api::LocalizationTable`
- `augur_plugin_api::LocalizationRow`
- `augur_plugin_api::CTX_LOCALIZATION_RESULTS`

With:

- `augur_plugin_types::localization::Localization`
- `augur_plugin_types::localization::LocalizationResults`
- `augur_plugin_types::localization::LocalizationTable`
- `augur_plugin_types::localization::LocalizationRow`
- `augur_plugin_types::localization::CTX_LOCALIZATION_RESULTS`

### Removed Reconstruction Hook

Remove any implementation of:

```rust
fn accumulated_localizations(&self) -> Option<Vec<u8>>
```

Replace with normal host-view publication:

- declare a dataset in `host_views()`
- return bytes from `host_view_dataset(dataset_id)`
- bump `host_view_dataset_generation(dataset_id)` when the serialized dataset changes

### New Capability Contract

Retained event history is now explicit. If a plugin needs host-retained history:

```rust
fn capabilities(&self) -> PluginCapabilities {
    PluginCapabilities {
        retained_event_history: true,
    }
}
```

If a plugin does not opt in, `augur-gui` will not retain frame history.

`PluginInput::RawEvents` still means current-frame raw events only.

### New Generic Dataset/View Kinds

New dataset kinds:

- `HostDatasetKind::Image2dV1`
- `HostDatasetKind::Series1dV1`

New view kinds:

- `HostViewKind::Scatter2dFromTable`
- `HostViewKind::ImageWindow`
- `HostViewKind::LineSeriesWindow`

---

## Verification

```bash
cargo test -p augur-plugin-api
cargo test -p augur-gui host_view
cargo test -p augur-gui event_store
cargo check -p augur-gui
```
