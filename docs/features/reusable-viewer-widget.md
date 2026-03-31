# Reusable Viewer Widget

## Summary

`augur-gui` now extracts the full central viewer panel into `viewer_widget.rs`, so the same
viewer component can render inside the main window or inside the popup viewport without diverging
toolbar, canvas, replay, or lower-control behavior.

## Scope

The shared viewer now owns:

- the heading and camera-info strip
- the 2D toolbar and 3D point-cloud controls
- the main canvas or placeholder surface
- replay transport controls
- the scrollable lower controls area for preview mode, scale bar, annotations, stats, warnings,
  notices, and errors
- histogram and line-profile auxiliary windows through one `ViewerState`

The host app still owns pipeline lifecycle, replay controller actions, config writes, analysis
execution, and texture generation.

When the shared viewer runs in the popup deferred viewport, it also requests its own repaint
cadence during live preview or replay and explicitly wakes the root viewport when it queues
host-owned actions such as replay transport changes. That keeps the popup transport controls
responsive even when focus is in the external window.

## Host Contract

`viewer_widget.rs` exposes three core types:

- `ViewerState` for viewer-owned mutable state such as view mode, zoom/pan, tools, contrast,
  annotations, scale bar, and auxiliary windows
- `ViewerInput` for per-frame host data such as texture handles, frames, pipeline stats, replay
  status, warnings, and error strings
- `ViewerOutput` for host actions such as ROI commits, popup toggles, hotpixel masking, replay
  transport commands, and preview-rerender triggers

This keeps `CameraApp` in charge of side effects while the viewer module owns the UI tree.

## Popup Behavior

Only one viewer is active at a time.

- in the embedded path, `CameraApp` renders `draw_viewer()` with its local `ViewerState`
- when the popup opens, the full `ViewerState` moves into shared popup data
- the main window shows a placeholder plus a return button while the popup is active
- when the popup closes, the same `ViewerState` moves back to the main host with zoom, tool,
  annotation, contrast, and replay-UI state preserved

This replaces the old popup-specific mini-renderer with the same viewer component used in the main
window.

## Reconstruction Follow-Up

This extraction is the prerequisite for the reconstruction-window branch work: the follow-up can
wrap the shared viewer with reconstruction-specific data preparation and export controls instead of
forking another copy of the central panel UI.

## Files

| File | Role |
|---|---|
| `augur-gui/src/viewer_widget.rs` | shared viewer state, inputs/outputs, toolbar/canvas/replay/control rendering |
| `augur-gui/src/app.rs` | host-side viewer orchestration, popup handoff, pipeline integration, and side effects |
| `augur-gui/src/viewer_tools/` | histogram, line profile, ruler, annotation, and scale-bar helpers owned by `ViewerState` |
| `docs/adr/011-reusable-viewer-widget.md` | architectural rationale for the extracted viewer-host boundary |
