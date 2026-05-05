# Clickable 2D Overlays and Failed-Fit Inspection

## Summary

Plugin marker overlays now carry an optional `source_row = (dataset_id, row_id)`
that the host uses as the authoritative selection key on click. When a user
clicks a diamond marker produced by `evesmlm-fitting` for a rejected fit, the
host selects the matching row in the `rejected_fits` dataset, triggers the
Stage 2 dim/bright emphasis across the 2D and 3D views, and auto-seeks to the
fit's anchor timestamp — reusing the existing row-selection pipeline without
any overlay-specific branching.

The reason-first inspector is a direct consequence of Stage 2: the summary
card renders the `headline`-flagged `rejection_reason` column as its heading
whenever a `rejected_fits` row is selected.

## Plugin Contract

Additive fields on `FfiMarkerOverlayItem` (plugin-api ABI bumped from 3 → 4):

- `source_dataset_id: FfiString` — optional (`FfiString::empty()` means "fall
  back"). The dataset whose row backs this marker.
- `source_row_id: FfiString` — optional. The row id within that dataset.

Bridged to `MarkerOverlayItem::source_row: Option<(String, String)>` in
`augur-core`. When both FFI strings are non-empty the pair is propagated;
otherwise the host falls back to `(overlay.dataset_id, marker.stable_id)` — so
plugins that have not opted in keep working.

## Host Behavior

- `pick_overlay_candidate` (`augur-gui/src/viewer_widget.rs`) prefers
  `marker.source_row` over the layer's dataset id when building the
  `StableRowKey`. Cross-dataset links are now expressible without the marker
  layer and the source row needing to share a dataset id.
- Row selection flows through the existing Stage 2 path
  (`App::handle_viewer_output` → `set_single_selection` +
  `maybe_auto_seek_to_row`), so clicking a rejected diamond seeks the replay
  to the fit's anchor timestamp and dims non-contributing events in the 3D
  cloud.
- Cross-span persistence comes for free from Stage 2: rejected-fit rows carry
  `provenance` with `span_start_column`/`span_end_column`, so scrubbing within
  the fit's span keeps the diamond visible without a per-frame cache.

## Code References

| Path | Role |
| --- | --- |
| `augur-plugin-api/src/ffi.rs` | `FfiMarkerOverlayItem::{source_dataset_id, source_row_id}`, `PLUGIN_ABI_VERSION = 4` |
| `augur-core/src/analysis/mod.rs` | `MarkerOverlayItem::source_row` |
| `augur-gui/src/plugin_loader.rs` | FFI → `MarkerOverlayItem::source_row` bridge in `add_marker_overlay` |
| `augur-gui/src/viewer_widget.rs` | `pick_overlay_candidate` precedence, regression test `overlay_picker_prefers_explicit_source_row_over_layer_dataset_id` |
| `augur-plugins/plugins/evesmlm-fitting/src/lib.rs` | Populates `source_row` on accepted crosses and rejected diamonds |
| `augur-plugins/plugins/evesmlm-postproc/src/lib.rs` | Populates `source_row` on its localization crosses |

## Related

- [Investigation Table Trustworthiness](./investigation-table-trustworthiness.md)
- [Investigation Workspace](./investigation-workspace.md)
- ADR 017: Declarative TableV1 Provenance and Display Metadata
