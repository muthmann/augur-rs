# Investigation Table Trustworthiness

## Summary

Plugin-owned `TableV1` datasets are rendered through a host-owned pipeline that preserves row
identity across frames, formats cells consistently, surfaces pagination, and keeps 2D, 3D, and
plugin tables aligned to the same replay timestamp on selection. When a derived row declares
relations back to an event dataset, the host now follows those joins to re-emphasize the
contributing raw events in 3D instead of only highlighting a coincident centroid.

Earlier passes left the investigation workspace visually noisy, timestamps unreadable, row
selection transient, and results anchored to frame indices instead of timestamps. This feature
tightens the host rendering layer and the plugin contract so that scrubbing replay shows the
same event in every linked view.

## Plugin Contract

Additive fields on `TableSchema` (all optional, serde-skipped when absent):

- `provenance: Option<TableRowProvenance>` — `anchor_time_column`, `span_start_column`,
  `span_end_column`. Used by the host to resolve an anchor timestamp and a visibility span for
  each row without hard-coded column conventions.
- `column_display: Vec<TableColumnDisplayEntry>` — per-column display metadata:
  - `label` overrides the column title in the UI
  - `format` is one of `TimestampMicros`, `FixedPrecision { digits }`, `Identifier`, `Category`
  - `width_priority` drives the default column width (`High`/`Medium`/`Low`)
  - `hidden` suppresses the column from headers and summary grids
  - `headline` promotes the column as the summary-card heading
- `relations: Vec<HostDatasetRelation>` on `HostDatasetDescriptor` — declares cross-dataset row
  joins (`via_column` on this dataset → `target_column` on `target_dataset_id`). The host uses
  these joins transitively when it needs to resolve a selected derived row back to raw-event
  identities.

Plugins opt in additively. Missing metadata falls back to prior behavior.

## Host Rendering

- **Timestamp formatting** — `TimestampMicros` renders `mm:ss.uuu` relative to the replay's
  `first_timestamp_us`; raw microseconds appear on hover. See
  `augur-gui/src/host_views.rs::format_timestamp_micros`.
- **Summary card** (`HostViewKind::CompactTable`) — replaces the old compact preview. Shows row
  count, action row (`Open full table`, `Export CSV`), the headline column as a heading, and a
  non-hidden column grid for the selected row. See `render_summary_card`.
- **TableWindow upgrade** — `egui_extras::TableBuilder` with resizable columns, width priority
  from `column_display`, sortable headers, and a pagination toolbar (25/100/500/All per page plus
  ◀/▶ navigation). See `render_linked_table_view`.
- **Auto-seek on row selection** — when replay is paused, selecting a row seeks the transport to
  that row's anchor timestamp (`TableSchema::row_anchor_timestamp_us`, fallback chain
  `anchor_time_column` → span midpoint → `time_column`). Wired at viewer, table, and 3D
  selection sites via `App::maybe_auto_seek_to_row`.
- **Span-based visibility** — overlays and the 3D point cloud hide rows whose
  declared span does not overlap the current frame's `[window_start_us, window_end_us]`. Rows
  without provenance are still shown, preserving behavior for plugins that have not opted in.
  See `retain_rows_in_frame_span` in `augur-gui/src/investigation.rs`.
- **Relation-driven 3D emphasis** — selecting a localization, rejected fit, or other derived row
  resolves through declared `HostDatasetRelation`s until the host reaches accepted-event rows with
  stable event identities. The 3D raw-event layer then brightens exactly those contributing events
  instead of relying on a `(timestamp, x, y)` coincidence guess.
- **Export CSV** — surfaced on every TableV1-backed panel with a UI (summary card, TableWindow,
  Density2d, Scatter2d).

## Parity

The EVE fitting and postproc plugins re-export the same `TableSchema` registry builder for
`current_localizations`; a runtime test in `evesmlm-postproc` asserts JSON-serialized
descriptor equality across the two plugins to catch accidental divergence.

## Code References

| Path | Role |
| --- | --- |
| `augur-plugin-api/src/context.rs` | `TableRowProvenance`, `TableColumnDisplayMetadata`, `TableColumnWidthPriority`, `TableSchema::row_anchor_timestamp_us`, `row_span_us` |
| `augur-gui/src/host_views.rs` | `format_cell_value`, `render_summary_card`, `render_linked_table_view`, `TableWindowViewportData` |
| `augur-gui/src/investigation.rs` | `TablePageSize`, `InvestigationTableViewState`, `row_key_for_row` |
| `augur-gui/src/app.rs` | `maybe_auto_seek_to_row`, table-window branches wiring auto-seek and pagination |
| `augur-plugins/plugins/evesmlm-fitting/src/lib.rs` | Declares provenance + display metadata for `current_localizations` and `rejected_fits` |
| `augur-plugins/plugins/evesmlm-postproc/src/lib.rs` | Descriptor parity test |

## Related

- [Host View Registry](./host-view-registry.md)
- [Investigation Workspace](./investigation-workspace.md)
- ADR 017: Declarative TableV1 Provenance and Display Metadata
