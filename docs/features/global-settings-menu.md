# Global Settings Menu and Replay Pacing

## Summary

`augur-gui` now centralizes host-owned runtime settings in a top-bar `Settings` menu instead of
splitting them across the `Camera` menu and the left settings panel.

This pass adds a persisted `[global]` config block, publishes shared `GlobalSettings` into the
runtime plugin context bus, fixes replay-speed changes so they reset pacing baselines instead of
causing jumps or long freezes, and keeps acquisition time editable during replay so the next replay
frame uses the new accumulation window.

## User-Facing Changes

- The top bar now includes `Settings` between `Camera` and `View`.
- `Acq time [ms]` moved out of `Camera` into `Settings`.
- The EventStore memory budget moved out of the left settings panel into `Settings`.
- `Settings` also owns pixel scale, sensor geometry, preview cadence, point-cloud cadence, and the
  recording disk-writer buffer size.
- Sensor geometry and disk-writer buffer are idle-only controls because they shape a pipeline at
  start time. Pixel scale, replay/runtime history budget, preview cadences, and acquisition time
  update immediately when the underlying pipeline/controller supports it.

## Persisted Config

`CameraConfig` now includes a backward-compatible `[global]` section:

```toml
[global]
nm_per_pixel = 4860.0
sensor_width = 1280
sensor_height = 720
acq_time_ms = 50
event_store_budget_mib = 100
preview_interval_ms = 33
point_cloud_interval_ms = 67
disk_writer_buffer_mib = 4
```

Older TOML files without `[global]` still load through serde defaults.

The GUI synchronizes those values when it:

- starts a live pipeline
- saves or loads a config file
- loads replay sidecars
- restores live state after replay closes

## Parameter Reference

| Parameter | Guidance |
|---|---|
| `nm_per_pixel` | Physical size of one sensor pixel in nanometers. Default: IMX636 sensor pitch `4860` nm (`4.86 µm`). Shared with plugins for coordinate conversion and used by the ruler/scale bar. In optical setups with magnification, replace it with the effective sample-plane calibration. |
| `sensor_width`, `sensor_height` | Sensor pixel dimensions. They must match the connected camera, default to IMX636 (`1280x720`), and are used for ROI validation plus plugin coordinate systems. These are idle-time settings because they describe the pipeline shape. |
| `acq_time_ms` | Duration of each preview frame's accumulation window. Lower values give finer temporal resolution but fewer events per frame; higher values integrate more events for a brighter preview while reducing temporal detail. |
| `event_store_budget_mib` | Maximum memory for retained decoded-event history. Runtime plugins can access past frames from this buffer, so increase it for longer analysis windows or reduce it to save RAM. |
| `preview_interval_ms` | Maximum redraw interval for the 2D preview. Lower values look smoother but cost more CPU/GPU work. This does not affect recording or replay timing. The default `33` ms is about `30` fps. |
| `point_cloud_interval_ms` | Maximum redraw interval for the 3D point-cloud view. Lower values are smoother but more GPU-intensive. The default `67` ms is about `15` fps. |
| `disk_writer_buffer_mib` | Write buffer size for the recording output file. Larger buffers reduce disk I/O pressure during high-bandwidth recordings but use more memory. This stays idle-only because the recording pipeline allocates it at startup. |

## Plugin Context Contract

Runtime plugins can now read host-owned experiment settings from the normal per-frame context bus:

- key: `augur.global_settings`
- type: `augur_plugin_api::GlobalSettings`

The published payload includes:

- `nm_per_pixel`
- `sensor_width`
- `sensor_height`
- `acq_time_ms`
- `event_store_budget_bytes`

This intentionally uses the existing string-keyed JSON context path, so no runtime FFI/vtable
change was needed for the new contract.

## Replay Behavior

`ReplayControls` now tracks a `speed_epoch` alongside `speed_bits`.

When replay speed changes, both replay backends reset their local pacing baseline:

- raw replay resets from the current byte position
- decoded replay resets from the current event index

That keeps the approximate byte-rate throttle responsive without misinterpreting already-played
data at the new speed.

Replay also now keeps `Acq time [ms]` enabled in the top-bar `Settings` menu. The GUI already
stores the value through `PipelineController::acq_time_us`, and the preview pipeline samples that
atomic on each frame boundary, so replay-time edits take effect on the next decoded frame window
without rebuilding the replay session.

## Files

| File | Role |
|---|---|
| `augur-plugin-api/src/context.rs` | shared `GlobalSettings` payload and key |
| `augur-core/src/config.rs` | persisted `[global]` config block with defaults |
| `augur-core/src/pipeline.rs` | configurable disk-writer buffer |
| `augur-core/src/replay.rs` | raw replay speed-epoch pacing reset |
| `augur-core/src/decoded_replay.rs` | decoded replay speed-epoch pacing reset |
| `augur-gui/src/app.rs` | top-bar `Settings` menu, save/load sync, plugin-context publication |
| `augur-gui/src/settings.rs` | ROI/mask controls now use editable sensor geometry |

## Verification

- `cargo fmt --all`
- `cargo test -p augur-plugin-api`
- `cargo test -p augur-core`
- `cargo test -p augur-gui`
- `cargo clippy --workspace`
- `cargo check -p augur-gui`
