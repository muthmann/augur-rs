# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with live preview, file replay, runtime controls, and plugin-driven analysis.

## Start the App

```bash
cargo run --bin augur-gui
```

## Menu Bar

A single menu bar row at the top of the window:

- `File` — output path/browse, Open Replay, Save/Load Config, Close Replay
- `Camera` — Probe Camera, Preview, Record, Stop, Apply Settings; replay mode adds Play/Pause, Restart, and Speed selection
- `Settings` — pixel scale, sensor geometry, Acq time, EventStore budget, and advanced preview / point-cloud / disk-writer controls
- `View` — toggle Settings/Analysis panels, switch 2D/3D view mode
- `Plugins` — Plugin Manager, Scan for New Plugins, Open Plugins Folder
- `Analysis` — per-plugin enable/disable checkboxes (shown only when plugins exist)

The right side of the menu bar shows the `2D / 3D` view-mode toggle, status indicators (● REC, Finished), and the current camera/session status label.

## Settings Panel

The left panel groups camera-only controls into focused sections:

- Biases
- ROI
- Pixel Mask (DEM)
- Digital Filters

### Live sessions

- `Idle`: edits affect the next preview or recording session
- `Previewing` / `Recording`: edits stay local until `Apply Settings`
- `Lock settings while recording` prevents runtime changes during capture

### Replay sessions

- the panel becomes read-only
- a companion `<capture>.toml` sidecar is shown when available and also seeds the top-bar `Settings` menu
- otherwise the GUI falls back to a geometry-matched default reference config
- collapse and expand happen from the panel edge, and the panel body is scrollable

## Analysis Panel

When at least one analysis plugin is enabled, a right-side **Analysis Tools** panel appears.

- plugins keep running even if the panel is hidden
- disabling all plugins hides the panel automatically
- built-in ROI Grid settings still edit `CameraConfig` directly
- runtime-loaded plugins expose declarative settings and status entries through the FFI host
- plugins can exchange per-frame derived data through the shared context bus
- the full panel body is scrollable and collapses from the panel edge

Available plugin types include hotpixel detection, ROI grid analysis, molecule localization, and focus metrics.

## Preview Workspace

The center panel is now an interactive preview workspace.

- hover the 2D preview to read sensor-space `x, y`
- `Select ROI` enables rectangular drag-to-select ROI editing
- `+`, `-`, `Fit`, and drag-panning control the zoomed view
- `Crop to ROI` switches between full-frame and ROI-only rendering
- `Enlarge` opens a larger popup that shares the same preview state
- `View -> 3D` replaces the image preview with a raw-event point cloud

The 3D view exposes controls for time range and max render limit, plus camera reset. Drag orbits the point cloud and the mouse wheel zooms it.

## Plugin Manager

Use the toolbar **Plugins** menu to open the **Plugin Manager** window.

- the default plugin directory is `~/.augur/plugins/`
- each plugin lives in its own subdirectory with a `plugin.toml` manifest plus one `.dylib`, `.so`, or `.dll`
- `Scan for New Plugins` refreshes the loader without restarting the GUI
- `Reload` unloads and reloads one plugin, which is useful while iterating on plugin code
- load failures stay visible in the manager instead of crashing the app

## Replay Mode

Opening a `.raw` file starts a preview-only pipeline backed by the file replay camera in `augur-core`.

Replay adds a visible transport bar between the canvas and the scrollable controls:

- `▶` / `⏸` Play/Pause, `⏮` Restart, `⏹` Stop buttons
- Speed combo box (`0.25x` to `Max`)
- a full-width timeline slider for seeking
- current / total replay time

Enabled analysis plugins continue to process replayed frames through the same `PreviewFrame` path used for live preview.

When replay reaches EOF, the pipeline threads stop but the app stays in replay mode so the final frame and controls remain available for restart or seeking.

## Preview Contrast

A `Contrast` slider below the preview is available in 2D mode only (live and replay).

- it uses percentile-based normalization instead of raw max-value normalization
- this keeps single hotpixels from washing out the rest of the frame
- lower percentiles reveal dimmer activity sooner

## Status Colors

Status, warning, and error labels adapt to the active GUI theme.

- warning and error text follows egui's theme-aware foreground colors
- replay and plugin success labels use a darker mid-green that stays readable in light mode
- analysis info messages use a darker blue for better contrast on light backgrounds

## Runtime Notes

- acquisition time is only adjustable for live preview and recording
- sensor geometry and disk-writer buffer are idle-only controls because they shape pipeline startup
- EventStore budget and preview/point-cloud cadence update immediately from the `Settings` menu
- output path editing is disabled during active recording and replay
- collapsing either side panel gives more space back to the preview
- the same embedded/popup preview controls work in both live and replay sessions
