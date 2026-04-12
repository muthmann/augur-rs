# Investigation Workspace

## Summary

`augur-gui` now exposes a host-owned investigation workspace instead of treating 2D preview and
3D inspection as separate, weakly connected modes.

This pass adds:

- a central `InvestigationState` in `augur-gui`
- three reusable layouts:
  - `2D only`
  - `split 2D + 3D`
  - `3D only`
- a WGPU-backed generic 3D inspection view with orbit, pan, zoom, focus, clip slab, and axis
  presets
- host-owned linked selection between preview markers, linked tables, and 3D points
- stable row identity through additive dataset metadata in `augur-plugin-api`
- generic rich marker overlays through `Overlay::MarkerOverlay` plus the plugin-side
  `HostOutput::add_marker_overlay(...)` bridge
- host-owned layer styling and visibility controls keyed by dataset/layer id instead of plugin name
- the same layout model for the embedded viewer and the popup viewer
- a right-side investigation inspector for layout, ROI linking, selection state, layer visibility,
  style controls, stage cards, and stale/provenance signals
- raw-event 3D history controls directly in the 3D toolbar, plus tooltip coverage for the exposed
  3D controls and layer actions
- raw-event continuity across drained preview frames so the 3D history no longer shows avoidable
  temporal holes when the 2D preview coalesces intermediate frames
- raw-event depth scaling against the history that is actually retained, so requesting more history
  than the buffer currently holds does not compress the existing cloud into a tiny time box
- a 3D ROI focus volume for linked raw-event inspection: points inside the ROI stay emphasized
  while surrounding points remain visible with low opacity instead of disappearing outright

The host remains generic. It only understands datasets, rows, coordinates, layers, and display
metadata. Plugin-specific research semantics are still outside this repository.

## UI Behavior

- The top bar now switches investigation layouts instead of the old 2D/3D mode toggle.
- The central viewer can show:
  - the existing 2D preview widget
  - the new 3D inspection surface
  - both side by side with a persistent split ratio
- Popup viewing reuses the same layout model and linked investigation state.
- The replay transport strip now stays available in the same top position in the popup/external
  viewer instead of disappearing outside the main window.
- The split layout now uses a visible draggable divider in both the embedded and popup viewers.
- Narrow split panes now keep both the 2D preview and the 3D inspection surface clipped to their
  actual pane rects instead of letting either pane reserve space under the right inspector or over
  the neighboring split pane.
- The 2D preview toolbar and 3D controls now scroll horizontally at narrow widths, so hover text,
  layer/status labels, and tool buttons no longer force the split panes wider than the divider
  allocation.
- Selecting a host-owned inspectable point in 2D highlights the same item in 3D and in linked
  tables.
- Selecting a point in 3D updates the same host selection state and focuses the camera target.
- Clicking a rich overlay marker in 2D now resolves into the same host selection model when the
  plugin provides stable ids, and falls back to the nearest linked dataset point otherwise.
- After plugin reset or replay re-analysis, the host now invalidates and rebuilds the host-view
  registry before the next investigation render, so 2D inspectable points keep their coordinate
  metadata instead of disappearing until a later refresh.
- When a boundary/highlight pixel overlaps a keyed marker overlay, the keyed marker now wins the
  2D hit-test so candidate clicks resolve into the linked table/3D selection flow instead of
  silently landing on an unkeyed outline pixel.
- Table windows filter against the active linked ROI, sort through the shared investigation table
  state, and auto-scroll toward externally selected rows.
- The investigation inspector shows:
  - a clearer `Workspace` section for ROI linking plus selected and hovered rows
  - grouped `Layers` controls that separate raw-event ON/OFF point-cloud styling from extra
    analysis layers
  - a `Status & warnings` section that lists concrete warning messages instead of only aggregate
    counts
  - a reminder that raw-event history controls live in the 3D toolbar
  - `Analysis Extensions` cards for host hotpixel detection plus enabled runtime plugins
  - stale host-setting indicators, notices, and errors
- Keyboard shortcuts:
  - `1` / `2` / `3` switch `2D only` / `split 2D + 3D` / `3D only`
  - `L` toggles 2D↔3D ROI linking
  - `Esc` clears the linked selection
  - `F` focuses the 3D camera on the current selection
- The 3D toolbar now exposes hover explanations for reset/fit/focus, depth slicing, point scale,
  history range, point budget, and axis presets.
- The raw-event history and point-budget controls now live in the 3D toolbar so the temporal
  context can be widened while inspecting the cloud, instead of forcing a trip through the right
  inspector.
- Raw-event point clouds now keep the events from every drained preview frame, not only the newest
  frame chosen for 2D rendering. This removes the avoidable time-axis packet gaps caused by the
  lossy preview queue.
- When the requested raw-event history is larger than the currently retained buffer, the 3D depth
  scale now uses the retained span that actually exists. The footer also reports both requested
  and retained history so sparse buffers are obvious instead of visually misleading.
- Raw-event 3D coordinates flip the sensor `y` axis so the cloud reads like the top-left-origin 2D
  preview: `x` still points right, `y` now points up, and older events extend deeper into the
  scene.
- When a linked ROI is active for raw events, the 3D view now draws a matching focus cuboid across
  the current retained time span. Points inside that volume stay emphasized, while outside points
  remain visible at low opacity for context.
- The 3D status text under the viewport now has a little more vertical room and a scrollable footer
  so layer, history, and control guidance does not clip away in shorter panes.
- When replay is paused, changing a hotpixel or plugin parameter reruns the current frame
  immediately so tuning feedback stays local and fast.

## Data Contracts

`augur-plugin-api` gained additive metadata so the host can link views without domain knowledge:

- `TableSchema.row_id_column`
- `TableSchema.coordinate_space_3d`
- `TableSchema.time_column`
- `TableSchema.layer_id`
- `TableSchema.semantic_label`
- `HostViewKind::Scatter3dFromTable`
- `HostDatasetDescriptor.display`
  - `layer_title`
  - `default_visibility`
  - `default_color`
  - `default_marker_shape`
  - `default_size`
- `Overlay::MarkerOverlay`
  - point, cross, box, ellipse, diamond, and filled-circle marker shapes
  - per-item color and size
  - optional timestamp
  - optional stable id
  - optional dataset id / layer id / semantic source label at the overlay level

The plugin FFI now exposes `HostOutput::add_marker_overlay(...)`, and the early-development plugin
ABI was bumped to require plugin rebuilds against the richer callback table.

When plugins omit these fields, the host falls back to generated row ids and dataset ids.

## Host Architecture Notes

- `augur-gui/src/investigation.rs` owns the generic investigation model and row/layer helpers.
- `augur-gui/src/inspection_3d.rs` owns the offscreen WGPU point renderer and 3D interaction.
- `augur-gui/src/viewer_widget.rs` now paints host-owned linked 2D markers on top of the existing
  preview image, including generic marker-overlay shapes and overlay hit-testing.
- `augur-gui/src/host_views.rs` now supports stable-id linked tables for host-rendered table
  windows.
- `augur-gui/src/point_cloud.rs` is now only a retained raw-event history buffer for the new 3D
  renderer; the old software-rasterized viewer-local 3D path is gone. The retained history now
  accumulates raw events from every drained preview frame so the 3D view does not inherit avoidable
  temporal holes from the 2D display-throttling path.

The host still avoids domain-specific analysis logic. The built-in raw-event layer is exposed as a
generic pair of host layers (`host.raw_events.on` and `host.raw_events.off`) instead of a
science-specific semantic type.

## Current Limits

- This pass now covers the layout shell, GPU 3D inspection, stable-id linking, rich overlay
  markers, paused replay reruns for current-frame tuning, per-layer style controls, raw-event
  history continuity, and linked ROI focus volumes in 3D.
- Explicit wider recompute scopes and generic export actions are still follow-up work.
- Existing dynamic plugins must be rebuilt against the current early-development ABI after the
  `add_marker_overlay` callback was added.

## Verification

```bash
cargo fmt --all
cargo test -p augur-core
cargo check -p augur-plugin-api
cargo check -p augur-gui --bin AugurRS
cargo test -p augur-plugin-api
cargo test -p augur-gui
AUGUR_RENDERER=wgpu cargo run --bin AugurRS --release
```
