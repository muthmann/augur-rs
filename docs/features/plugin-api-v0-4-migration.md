# Plugin API v0.4 Migration

## Who Needs This

Any runtime plugin built against the earlier `augur-plugin-api` must rebuild before loading into
this version of `augur-gui`.

The host still rejects stale plugins through the `PluginVTable::vtable_size` check. There is no
compatibility shim.

## Domain Payload Changes

Localization/SMLM payloads moved out of `augur-plugin-api`.

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

## Removed Reconstruction Hook

The old reconstruction-specific hook is gone.

Remove any implementation of:

```rust
fn accumulated_localizations(&self) -> Option<Vec<u8>>
```

Replace it with normal host-view publication:

- declare a dataset in `host_views()`
- return bytes from `host_view_dataset(dataset_id)`
- bump `host_view_dataset_generation(dataset_id)` when the serialized dataset changes

## New Capability Contract

Retained event history is now explicit.

If a plugin needs host-retained history, implement:

```rust
fn capabilities(&self) -> PluginCapabilities {
    PluginCapabilities {
        retained_event_history: true,
    }
}
```

If a plugin does not opt in, `augur-gui` will not retain frame history for it.

`PluginInput::RawEvents` still means current-frame raw events only.

## New Generic Dataset/View Kinds

New dataset kinds:

- `HostDatasetKind::Image2dV1`
- `HostDatasetKind::Series1dV1`

New view kinds:

- `HostViewKind::Scatter2dFromTable`
- `HostViewKind::ImageWindow`
- `HostViewKind::LineSeriesWindow`

Plugins can use these to expose richer, still host-owned analysis surfaces without extending the
ABI again.

## Not Included Here

This repository documents the ABI change but does not update sibling repositories such as
`augur-plugins` in this task. Those plugins must be rebuilt and adapted separately.
