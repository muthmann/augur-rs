# Interactive Preview Workbench

## Summary

`augur-gui` now treats the preview area as a shared workspace instead of a single passive image. The same preview state drives the main center panel and the enlarged popup, so zoom, crop, cursor readout, ROI selection, and the 2D/3D view toggle stay in sync while newer host-side viewer tools build on top of that workspace.

## 2D Preview Behavior

- hovering the preview shows the current sensor-space `x, y` cursor position
- the toolbar uses compact icon buttons with hover tooltips for ROI, histogram, line-profile, ruler, annotation, zoom, and popup actions, and it keeps the hover readout on the same row instead of wrapping
- `Select ROI` turns the preview into a rectangular drag tool that writes back to `CameraConfig::roi`
- zoom-out, zoom-in, and fit-to-window icon buttons control preview zoom, while drag-panning is available whenever the view is zoomed in
- `Crop to ROI` switches the 2D preview between full-sensor rendering and ROI-only rendering without changing the allocated canvas size; it crops to the selected software annotation when one is selected, otherwise it uses the hardware ROI
- the toolbar also owns the histogram toggle plus line-profile, ruler, and software-annotation tool modes
- `Esc` clears the active viewer-tool drawing state, and `Delete` / `Backspace` removes the selected annotation
- annotation list clicks now select the ROI immediately, switch back to pointer/edit mode, keep visible ROI numbering contiguous after deletes, and show the same per-ROI color used on the canvas
- in pointer mode, existing rectangle and ellipse annotations can be repositioned by dragging them directly on the preview
- histogram and line-profile tools can open in separate OS windows via deferred viewports, with embedded fallbacks on unsupported backends; the line-profile window opens after the drag completes
- `Enlarge` opens a native OS popup window (via `show_viewport_deferred`) that reuses the same preview state and includes replay controls; the main window shows a placeholder while the popup is open

The current configured ROI is outlined on the full-frame preview, and an in-progress ROI drag is drawn live over the image.

## 3D Point-Cloud View

The toolbar now includes a `View` toggle for `2D` versus `3D`.

The 3D mode:

- renders recent raw `CdEvent`s as a point cloud with `x`, `y`, and time as the three axes
- keeps a bounded recent-event history in `augur-gui` instead of changing the capture pipeline design
- requests raw events from the pipeline only while the 3D view is active or a plugin already needs them
- exposes controls for time range and max render limit, plus camera reset
- uses mouse drag for orbit and the mouse wheel for zoom
- filters the point cloud to the currently configured hardware ROI

## Side Panels

- the settings and analysis panels are now scrollable end to end
- collapse/expand controls live on the panel separator edges instead of the top toolbar
- collapsed panel arrows are frameless, small (14 px), and use weak text color for a minimal appearance; the collapsed strip is 22 px wide
- the analysis panel still only appears when at least one plugin is enabled

## Menu Bar Layout (UX Overhaul)

A single menu bar row replaces the former two-row layout:

- **Left**: `File | Camera | Settings | View | Tools | Plugins | Analysis` menus
  - `File` — output path/browse, Open Replay, Save/Load config
  - `Camera` — probe, preview, record, stop, apply settings; in replay mode also Play/Pause, Restart, and Speed selection
  - `Settings` — pixel scale, sensor geometry, acquisition time, retained event-history budget, and advanced preview / point-cloud / disk-buffer tuning
  - `View` — toggle side panels, switch 2D/3D, show/hide the scale bar
  - `Tools` — connect or disconnect the ImageJ/Fiji bridge
  - `Plugins` — plugin manager, scan, open folder
  - `Analysis` — per-plugin enable/disable checkboxes (shown only when plugins exist)
- **Right-aligned**: `2D / 3D` view-mode toggle, status indicators (● REC, Finished), ImageJ bridge status, and current camera/session status label

## Replay Transport Bar

When replaying a file, a dedicated transport bar appears between the canvas and the
scrollable controls area. The bar contains:

- **Play/Pause**, **Restart**, and **Stop** icon buttons
- a **Speed** combo box using `REPLAY_SPEED_OPTIONS`
- a full-width **timeline slider** for drag-seeking
- a **time label** showing `current / total` replay time

The transport bar replaces the old timeline-inside-scroll-area layout, giving the
slider full width and keeping playback controls always visible without scrolling.

## Central Panel — Bounded Image Height

The preview image (2D and 3D) is now capped so that a fixed height reserve
(`controls_reserve ≈ 190 px`) is always available for the colormap selector,
annotation stats, scale-bar controls, stats, and error rows. The transport bar sits above this reserve. When those controls
overflow the reserved area a vertical `ScrollArea` makes them reachable without
resizing the window.

`draw_preview_canvas` and `PointCloudState::draw` both accept a `max_height: f32`
parameter; the popup passes `ui.available_size().y` (unconstrained) while the main
panel passes `max_image_height`.

Preview placeholders, 2D overlay painters, and the 3D point-cloud painter now
intersect their canvas rects with the current `egui` clip rect. That keeps
hover overlays and background fills confined to the central panel when the
analysis side panel is present or resizing.

## Files

| File | Role |
|---|---|
| `augur-gui/src/app.rs` | shared preview workspace state, 2D preview controls, popup integration, panel arrows, toolbar layout |
| `augur-gui/src/point_cloud.rs` | recent-event history, orbit-camera math, and point-cloud painter rendering |
| `docs/gui.md` | user-facing GUI workflow documentation |

## Verification

- `cargo fmt --all`
- `cargo build -p augur-gui`
- `cargo test -p augur-gui`
- `cargo check -p augur-gui`
