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

## Loading Contract

Host-view callbacks now live directly on the flat `PluginVTable` exported through
`augur_plugin_vtable`. The host requires a current `augur-plugin-api` build and does not keep a
legacy ABI fallback path anymore.

## Host Rendering

`augur-gui` currently renders:

- compact analysis-panel tables with a 10-row preview cap
- read-only table windows with CSV export
- density maps derived from numeric table columns with zoom, contrast, colormap, image export, and provider-scoped `Clear`

Dataset payloads are fetched lazily and cached per dataset id only when at least one visible panel
or open window needs them.

The host also tracks `host_view_dataset_generation(dataset_id)` and only reloads a cached dataset
snapshot when the provider reports a newer generation. Density render state stays alive until
either the dataset generation changes or the view settings change.

## Verification

```bash
cargo fmt --all
cargo test -p augur-plugin-api
cargo test -p augur-gui
cargo build -p augur-gui
```
