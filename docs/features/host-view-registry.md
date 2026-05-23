# Host View Registry

## Summary

`augur-gui` resolves plugin-owned datasets into a host-owned registry of reusable analysis views.
Plugins describe what they can expose, while the GUI owns rendering, window state, cache
invalidation, and export behavior.

This is now the only supported path for plugin-owned analysis UI in this repository. The old
reconstruction-specific host hook is gone.

## Model

The registry has two plugin-facing layers:

- `HostDatasetDescriptor`: stable dataset id, title, kind, and empty-state message
- `HostViewDescriptor`: stable view id, dataset reference, placement, and host-rendered view kind

Current dataset kinds:

- `HostDatasetKind::TableV1`
- `HostDatasetKind::Image2dV1`
- `HostDatasetKind::Series1dV1`

Current view kinds:

- `HostViewKind::CompactTable`
- `HostViewKind::TableWindow`
- `HostViewKind::Density2dFromTable`
- `HostViewKind::Scatter2dFromTable`
- `HostViewKind::Scatter3dFromTable`
- `HostViewKind::ImageWindow`
- `HostViewKind::LineSeriesWindow`

## Resolution Rules

The host walks enabled runtime plugins in the same order used for frame
processing.

- later providers override earlier ones only when the descriptor metadata matches exactly
- conflicting duplicate ids are ignored and logged
- views whose dataset ids do not resolve are ignored and logged
- views whose kinds do not match the dataset kind are ignored and logged

The result is a resolved host-owned registry that is safe to render without plugin-specific UI
branches.

## Loading And Caching Contract

Host-view callbacks live directly on the flat `PluginVTable` exported through
`augur_plugin_vtable`.

The host requires a current `augur-plugin-api` build and does not keep any ABI fallback path.

Dataset payloads are:

- fetched lazily, only when a visible panel or open window needs them
- decoded into host-owned snapshot types
- cached by dataset id
- invalidated when `host_view_dataset_generation(dataset_id)` increases

Per-view render state is also host-owned. Density and image views keep their rendered textures
alive until either the dataset generation changes or the view settings change.

## Host Rendering

`augur-gui` currently renders:

- compact panel summaries inside the owning plugin card
- read-only table windows with CSV export
- density maps derived from numeric table columns, with zoom/contrast/colormap/image export
- scatter plots derived from numeric table columns, with CSV export
- generic 2D image windows with zoom/contrast/colormap/image export
- generic 1D line-series plots

`Scatter3dFromTable` is intentionally not dockable. The host uses those descriptors as
investigation 3D scene layers in the main split/3D viewer, so plugin cards and the View menu do not
offer them as separate host-view windows.

Exports stay generic:

- CSV export for table-backed views
- PNG/TIFF export for rendered image/density views

## Reconstruction Direction

Reconstruction is modeled as a generic host-view composition. A runtime plugin publishes a table dataset, and the host renders it through table, scatter, and density views.

This keeps `augur-gui` generic while supporting any analysis workflow that produces structured point data.

## Verification

```bash
cargo test -p augur-gui host_view
cargo test -p augur-plugin-api
cargo check -p augur-gui
```
