# ADR 017: Declarative TableV1 Provenance And Display Metadata

## Status

Accepted

## Context

`HostDatasetKind::TableV1` datasets drive the summary card, TableWindow, Density2d, Scatter2d,
and Scatter3d host views. Before this change:

- The host had no generic way to resolve a row's anchor timestamp or time span. Plugins and the
  host agreed on the `time_column` convention, but the host could not compute span-based
  visibility or auto-seek a selected row to the correct moment in replay.
- Cell rendering was type-based only. A `u64` timestamp, a `u64` row id, and an `f64`
  measurement were all rendered with the same formatting, producing noisy tables and unreadable
  microsecond columns.
- Cross-dataset links were implicit. A clustered-event row and its fitted localization had no
  discoverable join key from the host's perspective.

The earlier pass also left the GUI without pagination, with cramped inline tables, and with
result visibility anchored to frame indices instead of timestamps.

## Decision

Extend the plugin API additively so the host can render TableV1 datasets trustworthily without
per-plugin UI branches:

- Add `TableRowProvenance { anchor_time_column, span_start_column, span_end_column }` on
  `TableSchema`. The host resolves a row's anchor timestamp via fallback
  `anchor_time_column` → midpoint of span → `time_column`, and a span via
  `(span_start, span_end)` or `(anchor, anchor)`.
- Add `TableColumnDisplayMetadata` per column, carrying `label`, `format`
  (`TimestampMicros` | `FixedPrecision { digits }` | `Identifier` | `Category`),
  `width_priority`, `hidden`, and `headline`. The host uses these for cell rendering, column
  sizing, summary-card layout, and whether to hide purely structural columns.
- Add `HostDatasetRelation { target_dataset_id, via_column, target_column }` on
  `HostDatasetDescriptor` so the host can follow row-scoped joins (e.g. a candidate event's
  cluster_id to its fitted localization).
- All new fields are `#[serde(default, skip_serializing_if = "...")]` so the ABI stays
  compatible with existing plugins.

Route the host rendering layer through these fields:

- `render_summary_card` replaces the old compact-table preview. It uses `headline` for the
  heading and hides `hidden` columns from the summary grid.
- `render_linked_table_view` uses `egui_extras::TableBuilder`, sizes columns from
  `width_priority`, renders cells through a `format_cell_value` helper that understands the
  declarative formats, and adds a pagination toolbar (`TablePageSize` of 25/100/500/All).
- `App::maybe_auto_seek_to_row` seeks replay to the selected row's anchor when paused.

## Consequences

### Positive

- One host rendering path covers every current and future TableV1 dataset. Plugins opt in to
  better rendering declaratively.
- Selection is durable across frames and linked across 2D, 3D, and tables because anchor
  timestamps and stable row keys are authoritative.
- The host can evolve span-based visibility filtering, row-scoped actions, and cross-span
  persistence without further ABI churn.

### Negative

- Plugins that do not populate the new metadata continue to render with type-only formatting,
  which can look inconsistent next to plugins that do. Documented in the plugin authoring guide.
- The host carries more generic logic that was previously plugin-private. Tradeoff is explicit:
  rendering is host-owned, data shape is plugin-owned.

### Neutral

- JSON parity of identical schemas is now a property test concern (see `evesmlm-postproc`'s
  descriptor parity test against `evesmlm-fitting`).

## References

- [Investigation Table Trustworthiness](../features/investigation-table-trustworthiness.md)
- ADR 006: Host-Owned Dataset/View Registry for Plugin Outputs
- ADR 016: Host-Owned Generic Investigation Workspace
