# Interactive Preview Workbench

## Summary

`augur-gui` now treats the preview area as a shared workspace instead of a single passive image. The same preview state drives the main center panel and the enlarged popup, so zoom, crop, cursor readout, ROI selection, and the 2D/3D view toggle stay in sync.

## 2D Preview Behavior

- hovering the preview shows the current sensor-space `x, y` cursor position
- `Select ROI` turns the preview into a rectangular drag tool that writes back to `CameraConfig::roi`
- `+`, `-`, and `Fit` control preview zoom, while drag-panning is available whenever the view is zoomed in
- `Crop to ROI` switches the 2D preview between full-sensor rendering and ROI-only rendering
- `Enlarge` opens a resizable popup window that reuses the same preview state instead of creating a separate disconnected viewer

The current configured ROI is outlined on the full-frame preview, and an in-progress ROI drag is drawn live over the image.

## 3D Point-Cloud View

The toolbar now includes a `View` toggle for `2D` versus `3D`.

The 3D mode:

- renders recent raw `CdEvent`s as a point cloud with `x`, `y`, and time as the three axes
- keeps a bounded recent-event history in `augur-gui` instead of changing the capture pipeline design
- requests raw events from the pipeline only while the 3D view is active or a plugin already needs them
- exposes controls for time window, backward time offset, point limit, and camera reset
- uses mouse drag for orbit and the mouse wheel for zoom
- filters the point cloud to the currently configured hardware ROI

## Side Panels

- the settings and analysis panels are now scrollable end to end
- collapse/expand controls live on the panel separator edges instead of the top toolbar
- the collapsed panel buttons use `centered_and_justified` so the arrow is always
  fully visible regardless of platform DPI
- the analysis panel still only appears when at least one plugin is enabled

## Toolbar Layout (UX Overhaul)

The top bar was reorganised into two rows:

1. **Menu bar** — `File | Camera | View | Plugins | Analysis`
   - `File` — output path/browse, Open Replay, Save/Load config
   - `Camera` — probe, preview, record, stop, apply settings, acq time slider
   - `View` — toggle side panels, switch 2D/3D
   - `Plugins` — plugin manager, scan, open folder
   - `Analysis` — per-plugin enable/disable checkboxes (shown only when plugins exist)

2. **Compact mode-aware toolbar** — one row of buttons that changes with the active mode
   - Idle: `Probe Camera | Preview | Record | Open Replay`
   - Previewing: `Record | Stop | Apply Settings`
   - Recording: `Stop | ● REC`
   - Replaying: `Play/Pause | Restart | Stop Replay | Speed` combo
   - Always shows the `2D / 3D` view-mode toggle and the current status label

## Central Panel — Bounded Image Height

The preview image (2D and 3D) is now capped so that a fixed height reserve
(`controls_reserve ≈ 220 px`) is always available for the timeline, contrast
slider, stats, and error rows. When those controls overflow the reserved area a
vertical `ScrollArea` makes them reachable without resizing the window.

`draw_preview_canvas` and `PointCloudState::draw` both accept a `max_height: f32`
parameter; the popup passes `ui.available_size().y` (unconstrained) while the main
panel passes `max_image_height`.

## Files

| File | Role |
|---|---|
| `augur-gui/src/app.rs` | shared preview workspace state, 2D preview controls, popup integration, panel arrows, toolbar layout |
| `augur-gui/src/point_cloud.rs` | recent-event history, orbit-camera math, and point-cloud painter rendering |
| `docs/gui.md` | user-facing GUI workflow documentation |
| `book/src/gui.md` | mdBook GUI guide |

## Verification

- `cargo fmt --all`
- `cargo build -p augur-gui`
- `cargo test -p augur-gui`
- `mdbook build`
