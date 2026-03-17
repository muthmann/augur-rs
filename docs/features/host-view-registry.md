# Host View Registry

## Summary

`augur-gui` now resolves plugin-owned datasets into a host-owned registry of reusable analysis
views. Plugins describe what they can expose, while the GUI owns the rendering, window state, data
fetch timing, and export behavior.

This replaces the old reconstruction-specific host hook with a generic path that works for both
built-in plugins and runtime-loaded plugins.

## Model

The registry is split into two layers:

- `HostDatasetDescriptor`: stable dataset id, title, kind, and empty-state message
- `HostViewDescriptor`: stable view id, dataset reference, placement, and host-rendered view kind

Current table-oriented dataset and view kinds are:

- `HostDatasetKind::TableV1`
- `HostViewKind::CompactTable`
- `HostViewKind::TableWindow`
- `HostViewKind::Density2dFromTable`

## Resolution Rules

The host walks enabled built-in plugins first, then enabled runtime plugins, in the same order used
for frame processing.

- later providers override earlier ones only when the descriptor metadata matches exactly
- conflicting duplicate ids are ignored and logged
- views whose dataset ids do not resolve are ignored and logged

## Loading and Compatibility

Runtime plugins can now export ABI v2 through `augur_plugin_entry_v2`. The host:

- validates the ABI version, vtable size, and non-null vtable pointer
- prefers ABI v2 when present
- falls back to the legacy `augur_plugin_vtable` symbol when ABI v2 is absent

Legacy plugins still load, but they contribute an empty host-view registry.

## Host Rendering

`augur-gui` currently renders:

- compact analysis-panel tables with a 10-row preview cap
- read-only table windows with CSV export
- density maps derived from numeric table columns with zoom, contrast, colormap, image export, and provider-scoped `Clear`

Dataset payloads are fetched lazily and cached per dataset id only when at least one visible panel
or open window needs them.

## Verification

```bash
cargo fmt --all
cargo test -p augur-plugin-api
cargo test -p augur-gui
cargo build -p augur-gui
```
