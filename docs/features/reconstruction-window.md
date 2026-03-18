# Reconstruction Window

## Summary

`augur-gui` can open a dedicated reconstruction window that renders accumulated localization tables as a host-side super-resolution image and exports the underlying data as ThunderSTORM-style CSV plus PNG or TIFF images.

## Runtime Contract

The reconstruction window now reads the host-view dataset exposed by the active reconstruction
provider. It does not probe optional plugin hooks or manifest capabilities anymore.

## Expected Producer

The intended table producer is the external `Localization Reconstruction` runtime plugin from [augur-plugins](https://github.com/muthmann/augur-plugins). That plugin accumulates `augur.localization.results` from upstream localization/fitting plugins and exposes the full table through the dynamic-plugin API.

If no reconstruction provider is enabled, the window still opens but reports that no accumulated
localizations are available yet.

## Plugin Author Note

New plugins that want to feed the reconstruction window should:

1. declare a table dataset through `host_views()`
2. expose the dataset bytes through `host_view_dataset()`
3. register a density view that points at the dataset
