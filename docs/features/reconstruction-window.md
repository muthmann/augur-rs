# Reconstruction Views

## Summary

Reconstruction in `augur-gui` is modeled as a generic host-view composition:

- a plugin publishes a table dataset of points or measurements
- the host renders that dataset through generic host views
- export stays generic and host-owned

## Expected Producer

The expected producer is an external runtime plugin from [augur-plugins](https://github.com/muthmann/augur-plugins) that accumulates upstream results and exposes them as a host-view dataset.

## Recommended View Recipe

For reconstruction-like workflows, a plugin should usually publish:

1. one `HostDatasetKind::TableV1` dataset for the accumulated point table
2. one `HostViewKind::CompactTable` or `HostViewKind::TableWindow` for inspection/export
3. one `HostViewKind::Scatter2dFromTable` for direct point visualization
4. one `HostViewKind::Density2dFromTable` for rendered reconstruction density

This keeps the host generic and makes the same dataset reusable across multiple analyses.

## Export Behavior

The host now exposes only generic exports:

- CSV export for table-backed views
- PNG/TIFF export for rendered density or image views

There is no reconstruction-specific exporter in `augur-gui`.

## Consequence

If no reconstruction provider is enabled, no reconstruction views appear. Only plugin-declared host views are shown.
