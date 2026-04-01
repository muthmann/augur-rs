# Reconstruction Views

## Summary

`augur-gui` no longer owns a dedicated reconstruction window or reconstruction-specific runtime
path.

Reconstruction is now modeled as a generic host-view composition:

- a plugin publishes a table dataset of points or measurements
- the host renders that dataset through generic host views
- export stays generic and host-owned

## Expected Producer

The intended producer remains the external `Localization Reconstruction` runtime plugin from
[augur-plugins](https://github.com/muthmann/augur-plugins).

That plugin can accumulate
`augur_plugin_types::localization::CTX_LOCALIZATION_RESULTS` from upstream localization/fitting
plugins and expose the result as a normal host-view dataset.

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

There is no reconstruction-specific CSV writer or reconstruction-specific image exporter in
`augur-gui`.

## Consequence

If no reconstruction provider is enabled, there is no special empty reconstruction window anymore.
Only the plugin-declared host views appear.
