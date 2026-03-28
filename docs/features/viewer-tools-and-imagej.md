# Viewer Tools And ImageJ Bridge

## Summary

`augur-gui` now ships a host-owned viewer-tool layer on top of the shared preview workspace. The
2D preview can stay inside Augur for routine inspection work such as pixel probing, histogram-based
brightness/contrast tuning, line profiles, ruler measurements, software ROI annotations, ROI
statistics, colormap switching, and a scale-bar overlay. When a user wants a richer downstream
analysis surface, Augur can also hand the newest preview frames off to ImageJ/Fiji through a small
external-tool bridge backed by a bundled `AugurBridge.jar` plugin.

## Viewer Tools

- hover the preview to see `x`, `y`, ON, OFF, and combined pixel values
- the preview toolbar now uses compact icon buttons with hover tooltips instead of long text labels, and it stays on one row instead of wrapping the hover readout
- `Esc` clears the active ROI/line/ruler drawing mode, and `Delete` / `Backspace` removes the selected software annotation
- `Histogram` opens a combined-value histogram with labeled intensity/count axes, better hover readouts, and:
  - auto percentile mode
  - manual min/max range
  - gamma control
  - draggable min/max marker handles with an inline display ramp
  - a separate OS window when the backend supports deferred viewports, with an embedded fallback otherwise
- `Colormap` switches the 2D preview between:
  - polarity rendering (ON green / OFF red)
  - fire
  - grayscale
  - viridis
  - magma
  - inferno
- the scientific false-color LUTs now use exact canonical tables from matplotlib/ImageJ instead of coarse stop interpolation
- `Line Profile` samples ON and OFF values along a dragged line, opens its separate window after the drag completes, and can optionally overlay the ON+OFF sum trace
- `Ruler` measures dragged distances in both pixels and µm using the current pixel scale
- `Rect` and `Ellipse` create host-side software annotations with live ROI statistics
- `Scale bar` can be toggled from the UI and placed in any preview corner
- ruler/scale-bar text and line overlays now use outline/shadow rendering so they remain legible on grayscale and fire-like backgrounds

These tools are host-side inspection aids, not analysis plugins. They do not extend
`augur-core` or the plugin `Overlay` API.

## External Tool Bridge

The new `Tools -> Stream to ImageJ...` flow adds a small external-tool abstraction in
`augur-gui` and ships an initial ImageJ/Fiji implementation.

Modern ImageJ no longer exposes the old built-in socket listener, so this repository now includes a
tiny ImageJ plugin under `augur-gui/imagej-plugin/` that restores the simple local TCP `eval`
workflow Augur already uses.

- the dialog can export the bundled `AugurBridge.jar` directly, so users do not need to hunt for
  the file in the repository first
- in ImageJ/Fiji, install the jar with `Plugins -> Install PlugIn...`, or drag it onto the
  ImageJ/Fiji window, or copy it into the main `plugins/` folder
- do not place it in `plugins/Tools`, which ImageJ reserves for toolbar tools
- if menus do not refresh automatically, restart ImageJ/Fiji or run `Help -> Refresh Menus`
- then run `Plugins -> Augur -> Start Bridge`
- the bridge connects to a configurable `host:port` and now defaults to `127.0.0.1:57294`
- the dialog includes the plugin-install/start steps, inline status, and inline connection errors
- preview frames are written to a temporary TIFF path and forwarded to ImageJ on a lossy background
  channel so the GUI never blocks on the external consumer
- while streaming is active, the center panel shows a placeholder plus a `Return to augur` button
- the top bar exposes the current ImageJ bridge status

This bridge is intentionally lightweight and generic. ImageJ is only the first backend for the
`ExternalTool` trait.

## Architecture Notes

- viewer tools live under `augur-gui/src/viewer_tools/`
- preview rendering now uses explicit display settings (`min`, `max`, `gamma`, `colormap`) instead
  of a single percentile slider
- canonical LUT tables are embedded directly in `colormap.rs` for fire / viridis / magma / inferno
- interactive viewer-tool overlays are painted with `egui::Painter`, keeping the plugin
  `Overlay` enum reserved for analysis outputs
- histogram and line-profile windows use the same deferred-viewport pattern as the existing popup and
  host-view windows, so they can live outside the main OS window when supported
- because ImageJ replaced its historic socket listener with a Java-RMI single-instance mechanism,
  `augur-gui/imagej-plugin/AugurBridge.jar` restores a tiny loopback-only TCP listener instead of
  teaching the Rust bridge Java serialization/RMI
- the ImageJ bridge lives under `augur-gui/src/external_tools/` and uses a bounded background
  sender to avoid backpressuring capture or preview work

## Files

| File | Role |
|---|---|
| `augur-gui/imagej-plugin/` | bundled ImageJ plugin source, menu config, build script, and installable jar |
| `augur-gui/src/colormap.rs` | built-in preview colormaps and LUT generation |
| `augur-gui/src/preview.rs` | display-range-aware preview rendering plus combined histogram helper |
| `augur-gui/src/viewer_tools/` | histogram, line profile, ruler, annotations, and scale-bar state/helpers |
| `augur-gui/src/external_tools/` | generic external-tool trait plus ImageJ bridge |
| `augur-gui/src/app.rs` | menu wiring, preview toolbar/canvas interactions, tool windows, and bridge placeholder flow |
| `docs/gui.md` | user-facing GUI workflow updates |

## Verification

- `./augur-gui/imagej-plugin/build.sh`
- `cargo check -p augur-gui`
- `cargo test -p augur-gui`
- `cargo fmt --all -- --check`
- manual GUI pass still recommended for:
  - dragging each viewer tool in live and replay mode
  - checking histogram/manual contrast interaction feel
  - validating ImageJ/Fiji connectivity on a machine with the bundled `AugurBridge.jar` workflow
