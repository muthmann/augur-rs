# Viewer Tools And ImageJ Bridge

## Summary

`augur-gui` now ships a host-owned viewer-tool layer on top of the shared preview workspace. The
2D preview can stay inside Augur for routine inspection work such as pixel probing, histogram-based
brightness/contrast tuning, line profiles, ruler measurements, software ROI annotations, ROI
statistics, colormap switching, and a scale-bar overlay. When a user wants a richer downstream
analysis surface, Augur can also hand the newest preview frames off to ImageJ/Fiji through a small
external-tool bridge backed by a bundled `AugurBridge_.jar` plugin.

## Viewer Tools

- hover the preview to see `x`, `y`, ON, OFF, and combined pixel values
- the preview toolbar now uses compact icon buttons with hover tooltips instead of long text labels, and it stays on one row instead of wrapping the hover readout
- `Esc` clears the active ROI/line/ruler drawing mode, and `Delete` / `Backspace` removes the selected software annotation
- `Histogram` opens a mode-aware histogram with labeled intensity/count axes, better hover readouts, and:
  - auto percentile mode
  - manual min/max range
  - gamma control
  - draggable min/max marker handles with an inline display ramp
  - a separate OS window when the backend supports deferred viewports, with an embedded fallback otherwise
- `Mode` switches the 2D preview between:
  - red/blue polarity rendering with brightness from the summed ON+OFF count
  - signed net-count rendering through a blue-white-red diverging ramp
  - summed ON+OFF intensity through grays / fire / red hot / green / cyan hot / magenta hot / ice / blue-white-red
  - a time-surface view that decays each pixel by `exp(-(t_now - t_last) / τ)` with a configurable `Decay τ [ms]` slider
- host density views now use the same shared ImageJ LUT set as the 2D preview, minus the special polarity mode
- time-surface mode automatically requests raw preview events; if the current frame does not carry them yet, Augur falls back to grayscale intensity until the next raw-event frame arrives
- changing preview mode, time-surface decay, or histogram contrast now re-renders the current frame immediately instead of waiting for another decoded frame
- the shared false-color LUTs now use the requested official ImageJ ramps instead of the earlier preview-only matplotlib/ImageJ mix
- `Line Profile` samples ON and OFF values along a dragged line, opens its separate window after the drag completes, and can optionally overlay the ON+OFF sum trace
- `Ruler` measures dragged distances in both pixels and µm using the current pixel scale
- `Rect` and `Ellipse` create host-side software annotations with live ROI statistics
- `Scale bar` can be toggled from the UI and placed in any preview corner
- ruler/scale-bar text and line overlays now use outline/shadow rendering so they remain legible on grayscale and fire-like backgrounds
- the top-bar `Settings -> Advanced` controls now display preview and point-cloud update cadence in Hz instead of milliseconds

These tools are host-side inspection aids, not analysis plugins. They do not extend
`augur-core` or the plugin `Overlay` API.

## External Tool Bridge

The new `Tools -> Stream to ImageJ...` flow adds a small external-tool abstraction in
`augur-gui` and ships an initial ImageJ/Fiji implementation.

Modern ImageJ no longer exposes the old built-in socket listener, so this repository now includes a
tiny ImageJ plugin under `augur-gui/imagej-plugin/` that provides a loopback TCP bridge with both a
text command protocol and a binary frame protocol for live streaming.

- the dialog can export the bundled `AugurBridge_.jar` directly, so users do not need to hunt for
  the file in the repository first
- in ImageJ/Fiji, install the jar with `Plugins -> Install PlugIn...`, or drag it onto the
  ImageJ/Fiji window, or copy it into the main `plugins/` folder
- do not place it in `plugins/Tools`, which ImageJ reserves for toolbar tools
- if menus do not refresh automatically, restart ImageJ/Fiji or run `Help -> Refresh Menus`
- then run `Plugins -> Augur -> Start Bridge`
- the bridge connects to a configurable `host:port` and now defaults to `127.0.0.1:57294`
- the dialog includes the plugin-install/start steps, inline status, and inline connection errors
- preview frames are sent as raw 16-bit pixel data over the binary `frame` protocol; the plugin
  updates a single persistent `ImagePlus` window in-place via `setPixels()` + `updateAndDraw()`,
  so there is no close/reopen flicker and no TIFF file I/O per frame
- the bridge now keeps only the newest pending frame and streams it from a small background worker,
  so a slow ImageJ/Fiji update loop does not build up stale preview backlog inside Augur
- while streaming is active, the center panel shows a placeholder plus a `Return to augur` button
- the top bar exposes the current ImageJ bridge status

This bridge is intentionally lightweight and generic. ImageJ is only the first backend for the
`ExternalTool` trait.

## Architecture Notes

- viewer tools live under `augur-gui/src/viewer_tools/`
- preview rendering now uses explicit display settings (`min`, `max`, `gamma`) plus a
  `PreviewMode` enum that covers red/blue polarity, signed counts, intensity colormaps, and the
  host-side time-surface path instead of a single percentile slider plus `Option<Colormap>`
- `preview.rs` keeps a persistent per-pixel timestamp buffer for time-surface rendering and uses
  mode-aware histograms so auto-range matches the active visualization semantics
- preview histogram work is capped to a bounded bin count again, and time-surface mode now reuses
  one cached decay pass for both rendering and histogram generation instead of recomputing the same
  exponential values twice per frame
- the shared `colormap.rs` module now owns the ImageJ LUT tables for grays / fire / red hot /
  green / cyan hot / magenta hot / ice plus the blue-white-red diverging ramp used by signed-count
  rendering
- interactive viewer-tool overlays are painted with `egui::Painter`, keeping the plugin
  `Overlay` enum reserved for analysis outputs
- histogram and line-profile windows use the same deferred-viewport pattern as the existing popup and
  host-view windows, so they can live outside the main OS window when supported
- because ImageJ replaced its historic socket listener with a Java-RMI single-instance mechanism,
  `augur-gui/imagej-plugin/AugurBridge_.jar` restores a loopback-only TCP listener with a binary
  frame protocol (`frame <w> <h> <scale>\n` + raw u16 LE pixels) that updates a persistent
  `ImagePlus` in-place, avoiding TIFF file I/O and window close/reopen per frame
- the ImageJ bridge lives under `augur-gui/src/external_tools/` and uses a bounded background
  latest-frame sender plus fixed-size chunked pixel writes to avoid backpressuring capture or
  preview work with unnecessary whole-frame copies or per-frame serialization buffers

## Files

| File | Role |
|---|---|
| `augur-gui/imagej-plugin/` | bundled ImageJ plugin source, menu config, build script, and installable jar |
| `augur-gui/src/colormap.rs` | shared ImageJ colormaps and LUT generation for preview + host views |
| `augur-gui/src/preview.rs` | preview-mode-aware rendering, signed/time-surface helpers, and mode-aware histograms |
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
  - checking red/blue polarity, signed-count, intensity, and time-surface preview modes in live and replay mode
  - checking histogram/manual contrast interaction feel, including paused replay refreshes
  - validating ImageJ/Fiji connectivity on a machine with the bundled `AugurBridge_.jar` workflow
