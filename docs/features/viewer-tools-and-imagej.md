# Viewer Tools And ImageJ Bridge

## Summary

`augur-gui` now ships a host-owned viewer-tool layer on top of the shared preview workspace. The
2D preview can stay inside Augur for routine inspection work such as pixel probing, histogram-based
brightness/contrast tuning, line profiles, ruler measurements, software ROI annotations, ROI
statistics, colormap switching, and a scale-bar overlay. When a user wants a richer downstream
analysis surface, Augur can also hand preview frames off to ImageJ/Fiji through a small
external-tool bridge backed by a bundled `AugurBridge_.jar` plugin. That bridge now supports both a
bounded timeline stack for frame-history analysis and a live-only compatibility mode for the older
single-window overwrite workflow.

## Viewer Tools

- hover the preview to see `x` and `y` plus a mode-aware pixel readout: most modes show `ON`, `OFF`, and `Total`, while `Time Surface` shows the rendered decay `Value` and `Total`
- the preview toolbar now uses compact icon buttons with hover tooltips instead of long text labels, and it stays on one row instead of wrapping the hover readout
- `Esc` clears the active ROI/line/ruler drawing mode, and `Delete` / `Backspace` removes the selected software annotation
- `Histogram` opens a mode-aware histogram whose axes name what is actually counted — `Pixel value (ON+OFF events)`, `Pixel value (|ON − OFF| events)` or `Pixel value (time-surface decay, 0–255)` on x, `Pixels` on y — with a caption stating what one bin means in the active preview mode, plus:
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
  - its x axis is the **Euclidean distance from the line start in sensor pixels**, not the Bresenham sample index; on a diagonal the two differ by √2 and plotting the index would compress the axis by 41%
  - its y axis is labelled `Events per pixel`
  - a note above the plot states the line length in px and µm plus the sample count
- `Ruler` measures dragged distances in both pixels and µm using the current pixel scale
- `Rect` and `Ellipse` create host-side software annotations with live ROI statistics
- the annotations list is now directly interactive: clicking a row selects that ROI, switches back to pointer/edit mode, and shows a color swatch that matches the overlay on the preview
- pointer mode can move existing rectangle and ellipse annotations by dragging them directly on the preview
- visible ROI numbering in the annotations list now stays contiguous after deletes even though the underlying annotation IDs remain stable for selection/crop state
- `Crop to ROI` now crops to the selected software annotation when one is selected, otherwise it falls back to the current hardware ROI; toggling it again returns to the full frame, and ellipse crop uses the ellipse bounding box
- `Scale bar` can be toggled from the UI and placed in any preview corner
- **µm readouts state their provenance.** The scale bar, ruler and line-profile length note append `(uncal.)` until the pixel scale is confirmed for the optical setup — see [Pixel Scale Calibration](./pixel-scale-calibration.md) and ADR 033
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
- the bridge startup dialog now lets the user choose `Timeline (stack)` or `Live only (single frame)`
  and configure a persisted `Max frames` cap (default `500`) for timeline mode
- the bridge connects to a configurable `host:port` and now defaults to `127.0.0.1:57294`
- the dialog includes the plugin-install/start steps, inline status, and inline connection errors
- preview frames are sent as raw 16-bit pixel data over the binary `frame` protocol; the Rust side
  now tags each frame with a sequence number and frame-end timestamp so the ImageJ plugin can label
  stack slices consistently
- in `Timeline (stack)` mode, the plugin accumulates frames into a bounded `ImageStack`, exposes the
  normal ImageJ slice slider for history navigation, follows the newest slice only while the user is
  already on the live tail, and archives the current stack into a separate window if frame
  dimensions change mid-stream
- when the stack cap is reached, the oldest slices are dropped from the front so long runs stay
  bounded in memory
- in `Live only (single frame)` mode, the plugin preserves the previous behavior and keeps one
  persistent `ImagePlus` window updated in place via `setPixels()` + `updateAndDraw()`
- the Rust bridge now uses a bounded background frame queue (`32` envelopes) so brief ImageJ/Fiji
  stalls can absorb short bursts without backpressuring Augur's preview/capture path
- **overflow is counted, not silent.** When the queue is full the bridge drops the frame — stalling
  the preview would be worse — and increments a drop counter exposed through
  `ExternalTool::throughput() -> ExternalToolThroughput { frames_offered, frames_dropped }`:
  - the top-bar chip switches from `Success` to `Warn` tone and reads
    `ImageJ: Streaming · 37 dropped`
  - the `Stream to ImageJ` dialog shows a `Frames:` row (`1163 frames sent · 37 dropped (3.1%)`)
    and, once anything has been dropped, a line pointing at TIFF export or an analysis run for a
    complete series
  - the trait method has **no default implementation**, so a future bridge cannot opt out of drop
    accounting (ADR 033)
- the ImageJ plugin drains pending frames onto the EDT in batches instead of blocking the socket
  thread with one `invokeAndWait` call per frame
- while streaming is active, the center panel shows a placeholder plus a `Return to augur` button
- the top bar exposes the current ImageJ bridge status

This bridge is intentionally lightweight and generic. ImageJ is only the first backend for the
`ExternalTool` trait.

## Architecture Notes

- viewer tools live under `augur-gui/src/viewer_tools/`
- the full central viewer panel now lives in `augur-gui/src/viewer_widget.rs`, and both the main
  window and popup host render that same component from a shared `ViewerState` / `ViewerInput` /
  `ViewerOutput` contract instead of maintaining a popup-specific mini renderer
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
  frame protocol (`frame <w> <h> <scale> <seq> <timestamp_us>\n` + raw u16 LE pixels)
- the Java bridge keeps its own EDT-side frame queue so multiple incoming frames can be folded into
  one stack/display update, which keeps the ImageJ UI responsive under bursty traffic
- the ImageJ bridge lives under `augur-gui/src/external_tools/` and now uses a bounded background
  frame queue plus fixed-size chunked pixel writes so Augur can deliver short bursts of frame
  history without blocking capture or preview work
- the default ImageJ presentation is now a capped in-memory `ImageStack` instead of a single
  overwritten image window, but the plugin still exposes a live-only fallback for users who only
  need a remote preview

## Files

| File | Role |
|---|---|
| `augur-gui/imagej-plugin/` | bundled ImageJ plugin source, menu config, build script, and installable jar |
| `augur-gui/src/colormap.rs` | shared ImageJ colormaps and LUT generation for preview + host views |
| `augur-gui/src/preview.rs` | preview-mode-aware rendering, signed/time-surface helpers, and mode-aware histograms |
| `augur-gui/src/viewer_widget.rs` | reusable central viewer UI shared by the embedded host and popup viewport |
| `augur-gui/src/viewer_tools/` | histogram, line profile, ruler, annotations, and scale-bar state/helpers |
| `augur-gui/src/external_tools/` | generic external-tool trait plus ImageJ bridge |
| `augur-gui/src/app.rs` | menu wiring, popup handoff, preview rendering orchestration, and bridge placeholder flow |
| `docs/gui.md` | user-facing GUI workflow updates |

## Verification

- `./augur-gui/imagej-plugin/build.sh`
- `jar tf augur-gui/imagej-plugin/AugurBridge_.jar`
- `cargo check -p augur-gui`
- `cargo test -p augur-gui`
- `cargo fmt --all -- --check`
- manual GUI pass still recommended for:
  - checking red/blue polarity, signed-count, intensity, and time-surface preview modes in live and replay mode
  - checking histogram/manual contrast interaction feel, including paused replay refreshes
  - validating ImageJ/Fiji connectivity on a machine with the bundled `AugurBridge_.jar` workflow
  - checking timeline-stack scrubbing, auto-follow pause/resume, cap trimming, dimension-change archiving, and the live-only fallback mode
