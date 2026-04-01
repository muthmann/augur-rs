# Performance Overhaul

## Summary

This pass extends the earlier performance safeguards with deeper hot-path and plugin-host changes:

- split preview, disk, and raw buffer sizing in `augur-core`
- add queue/drop and blocked-writer telemetry
- reuse preview-frame image buffers instead of cloning full sensor images every frame
- render preview images directly as `Color32` without an intermediate RGBA buffer
- cache dynamic-plugin settings/status polling instead of re-reading JSON on every egui update
- open decoded replay files asynchronously so large `.csv`, `.bin`, `.npy`, `.h5`, and `.hdf5`
  files no longer freeze the GUI before playback starts
- switch the plugin API `EventStore` to segmented frame-based access
- add host-view dataset generations so the GUI can keep decoded datasets and density textures until
  the plugin says the data really changed
- add raw context publishing helpers for high-frequency plugin traffic

Recording correctness still takes priority over preview smoothness. Preview, point-cloud, replay,
host-view, and plugin work may coalesce or drop; the disk path must remain bounded and explicit.

## Capture and Preview Pipeline

- `augur-core/src/pipeline.rs` now has separate capacities for:
  - raw USB buffer pool
  - disk queue
  - preview packet pool/queue
  - preview frame queue
- Preview packet copies are now lazy. The USB reader only clones packet bytes when preview work is
  enabled and the preview queue/pool can accept them.
- Preview-frame pixel buffers are recycled through a pooled return path instead of cloning
  `pixels`, `pixels_on`, and `pixels_off` on every frame.
- `PipelineStatsSnapshot` now reports:
  - preview packet drops
  - preview frame drops
  - preview queue high-water marks
  - disk queue high-water mark
  - cumulative disk send wait time
  - cumulative disk write time

## GUI and Replay Behavior

- Preview rendering in `augur-gui/src/preview.rs` now writes directly into `ColorImage::pixels`
  using `Color32`, while reusing scratch storage for histogram and ROI-grid bookkeeping.
- The 3D point-cloud path now runs at a lower presentation cadence than the 2D preview path, which
  keeps point-cloud mode from forcing the same repaint pressure as texture mode.
- Dynamic-plugin settings/status UI reads are cached for 250 ms and invalidated immediately after
  runtime mutations such as `set_setting`, `set_enabled`, `reset`, reload, or rescan.
- Decoded replay opening is asynchronous. Raw `.raw` replay remains synchronous because it is
  already cheap to initialize.

## Plugin API / Host Changes

- `EventStore` is now segmented by frame instead of exposing one contiguous event slice.
- Plugins receive `FfiEventFrame` windows through `EventStoreHandle::frame()`, `frames()`,
  `frames_in_range()`, and `collect_events_in_range()`.
- `Plugin::host_view_dataset_generation(dataset_id)` lets the host keep cached dataset snapshots
  and host-rendered density textures until the generation changes.
- `HostContext::publish_raw` and `publish_persistent_raw` let plugins reuse their own serialized
  payloads instead of forcing host-side JSON serialization every frame.

## Breaking Change

This is a breaking runtime-plugin ABI change.

- `PluginVTable` gained `host_view_dataset_generation`
- `FfiEventStoreHandle` switched from contiguous event slices to frame-based callbacks
- runtime plugins must rebuild against the current `augur-plugin-api`

See the [Plugin API Migration History](./plugin-api-migration-history.md) for the exact adaptation notes.

## Verification

```bash
cargo fmt --all
cargo test -p augur-core
cargo test -p augur-plugin-api
cargo test -p augur-gui
cargo build -p augur-gui --release
cargo check -p augur-gui
```
