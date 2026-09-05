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

## Dock And Pop-Out Window Contract

Dockable views are shown either as tabs in the bottom host-view dock or as popped-out OS windows.
Three host-side rules keep that surface predictable:

- **The dock never claims space it does not own.** The dock clips its own contents
  (`clip_to_panel`) and scrolls its tab strip horizontally with the control cluster (pop out /
  maximize / collapse) reserved on the right. Without this a long tab strip paints across the
  analysis side panel — which is exactly what happened up to egui 0.31, where every panel inherited
  a screen-wide clip rect. egui 0.35 narrows that inherited rect itself; the explicit clip stays so
  the dock's own rect, not egui's default, defines the bound.
- **`dock_tabs` is user intent, not derived state.** Default tabs are seeded exactly once, since
  every analysis parameter change re-resolves the registry and re-seeding would resurrect views the
  user closed. Ids that do not currently resolve (plugin reload, epoch bump) are skipped while
  rendering instead of being pruned, so a momentarily empty registry cannot retire a tab for good.
- **Window requests travel through a long-lived cell.** A deferred viewport renders *after* the
  parent frame that registered it, so each popped-out view keeps one `Arc<Mutex<…ViewportData>>`
  (`HostViewWindowChannels`) that outlives a frame. The app republishes frame inputs into that cell
  and `HostWindowFrameData::carry_pending_requests_from` preserves requests the window raised in the
  meantime — close, freeze, CSV/PNG export, sorting, paging. A per-frame cell drops those requests
  on the floor, which is what made pop-out windows impossible to close. The window also wakes the
  root viewport (`request_root_repaint`) whenever a request is pending, because only the app
  services them.

## Chip, Tab, And Empty-State Presentation

- **Chips wrap as whole units.** The view-chip row uses `theme::wrap_row`, not
  `horizontal_wrapped`. The Analysis panel enables text wrapping for its whole subtree, and egui
  measures a `Button`'s label against the width left on the current line — so a chip that does not
  fit was breaking its label one character per line into a vertical strip instead of moving down.
  Shortened titles (`short_host_view_chip_title`) reduce how often the row fills up; they do not
  fix the wrap itself. Any new row of chips or buttons in a side panel must use `wrap_row`.
- **A dock tab shows the kind once.** The leading Phosphor glyph encodes the view kind; the textual
  kind tag lives in the tooltip alongside the dataset id. The close button appears only on the
  active or hovered tab, in a permanently reserved slot so revealing it does not reflow the strip.
- **Empty views centre their message.** `host_views::empty_state` renders a view's
  `empty_message` centred and muted in the space the data would occupy. A bare `ui.label` stranded
  it in the top-left corner of what is often a very tall empty dock.

## Reconstruction Direction

Reconstruction is modeled as a generic host-view composition. A runtime plugin publishes a table dataset, and the host renders it through table, scatter, and density views.

This keeps `augur-gui` generic while supporting any analysis workflow that produces structured point data.

## Verification

```bash
cargo test -p augur-gui host_view
cargo test -p augur-plugin-api
cargo check -p augur-gui
```
