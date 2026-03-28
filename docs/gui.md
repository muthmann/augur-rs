# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with live preview, file replay, runtime controls, and plugin-driven analysis.

## Start the App

```bash
cargo run --bin augur-gui
```

---

## Menu Bar

A single menu bar row at the top of the window:

| Menu | Contents |
|---|---|
| `File` | Output path/browse, Open Replay, Save/Load Config, Close Replay |
| `Camera` | Probe Camera, Preview, Record, Stop, Apply Settings; replay mode adds Play/Pause, Restart, and Speed selection |
| `Settings` | Pixel scale, sensor geometry, Acq time, EventStore budget, and advanced preview / point-cloud / disk-writer controls |
| `View` | Toggle Settings/Analysis panels, show/hide the scale bar, switch 2D/3D view mode |
| `Tools` | Connect or disconnect the ImageJ/Fiji bridge |
| `Plugins` | Plugin Manager, Scan for New Plugins, Open Plugins Folder |
| `Analysis` | Per-plugin enable/disable checkboxes (shown only when plugins exist) |

The right side of the menu bar shows the `2D / 3D` view-mode toggle, status indicators (● REC, Finished), the ImageJ bridge status when connected, and the current camera/session status label.

---

## Settings Panel (Left)

Camera-only controls are shown in the left panel.

- the panel collapses from a separator-edge arrow instead of the toolbar
- the full panel body is scrollable

### Live camera sessions

- In `Idle`, edits affect the next preview or recording session
- In `Previewing` or `Recording`, edits stay local until `Apply Settings`
- `Lock settings while recording` prevents runtime changes during capture

### Replay sessions

- the left camera settings panel becomes read-only
- if a companion `<capture>.toml` sidecar exists, both the panel and the top-bar `Settings` menu use it as replay reference data
- if no sidecar exists, the GUI shows a default geometry-matched reference config instead

### Camera sections

- Biases
- ROI
- Pixel Mask (DEM)
- Digital Filters

---

## Analysis Panel (Right)

When at least one analysis plugin is enabled, a right-side **Analysis Tools** panel appears.

- each enabled plugin gets a section in the panel
- built-in ROI Grid settings still edit `CameraConfig` directly
- runtime-loaded plugins expose declarative settings and status entries through the FFI host
- plugins can exchange per-frame derived data through the shared context bus
- hiding the panel does not disable plugin execution
- disabling all plugins hides the panel automatically
- the full panel body is scrollable
- collapse and expand happen from the panel edge, not the top toolbar

### Available plugin types

- Hotpixel detection
- ROI grid analysis
- Molecule localization
- Focus metrics

See [Plugin Architecture](./features/analysis-plugins.md) for the plugin host model.

## Plugin Manager

Use the toolbar **Plugins** menu to open the **Plugin Manager** window.

- the default plugin directory is `~/.augur/plugins/`
- each plugin lives in its own subdirectory with a `plugin.toml` manifest plus one `.dylib`, `.so`, or `.dll`
- `Scan for New Plugins` refreshes the loader without restarting the GUI
- `Reload` unloads and reloads one plugin, which is useful while iterating on plugin code
- load failures stay visible in the manager instead of crashing the app

---

## Preview Workspace

The center panel is now an interactive preview workspace shared by the embedded view and the enlarged popup.

### 2D preview tools

- hover the preview to read the current sensor-space `x, y` position together with ON / OFF / combined pixel values
- the preview toolbar uses compact icon buttons with hover tooltips for ROI, line profile, ruler, annotation, histogram, zoom, and popup actions, and it stays on one row instead of wrapping the hover readout below the preview
- `Histogram` opens a combined histogram plus brightness/contrast window with labeled intensity/count axes, hover readouts, Auto percentile, manual min/max, gamma controls, draggable marker handles, and a display-ramp preview
- the scroll area below the preview includes a `Colormap` selector for polarity, fire, grayscale, viridis, magma, and inferno rendering
- `Select ROI` turns the preview into a rectangular drag tool that writes back to `CameraConfig::roi`; during replay the disabled tooltip points users to rectangle annotations instead
- `Line Profile` samples ON and OFF intensity along a dragged line, opens after the drag completes, and includes labeled axes plus an optional ON+OFF sum trace
- `Ruler` measures dragged distances in both pixels and µm using the current pixel scale
- `Rect` and `Ellipse` add host-side software annotations; selecting one shows ROI statistics for ON, OFF, and combined channels
- `Esc` clears the active ROI/line/ruler/annotation draft, while `Delete` / `Backspace` removes the selected annotation
- zoom-out, zoom-in, and fit-to-window icon buttons control zoom
- when zoomed in, dragging the image pans the viewport
- `Crop to ROI` switches between full-frame and ROI-only rendering without changing the allocated canvas size
- a scale-bar overlay can be toggled from `View` or from the preview controls and positioned in any corner
- `Enlarge` opens a larger resizable popup that reuses the same zoom/crop/ROI state as the main preview

If a ROI is configured, the full-frame preview shows its outline. While dragging a new ROI, the pending selection rectangle is drawn live on top of the image.

### 3D point-cloud view

Switch the toolbar `View` control to `3D` to replace the image preview with a point cloud built from recent raw `CdEvent`s.

- `Time range [ms]` controls how far back in time to show events
- `Max render` caps the rendered sample count for performance
- `Reset Camera` resets the orbit camera
- drag the point cloud to orbit and use the mouse wheel to zoom

The point cloud always shows the most recent events and is filtered to the currently configured hardware ROI.

## Replay Mode

Opening a replay file starts a preview-only pipeline:

- `.raw` files use `RawFileCamera`
- decoded `.csv`, `.bin`, `.npy`, and optional `.h5` / `.hdf5` files use `DecodedEventFileCamera`
- all formats share the same transport controls, plugin path, and preview rendering
- the same replay session can be viewed in either 2D or 3D via the toolbar `View` toggle

Decoded replay files carry geometry differently:

- `.csv` requires a `%geometry:W,H` header
- `.bin` stores geometry in its binary header
- `.npy` infers geometry from `max(x) + 1` / `max(y) + 1` with a minimum of `1280x720`
- `.h5` / `.hdf5` read the file-level `geometry` attribute when present and otherwise infer geometry from event bounds with a minimum of `1280x720`

HDF5 replay is optional at build time. Build or run `augur-gui` with `--features hdf5` on a machine with the HDF5 system library installed to enable `.h5` / `.hdf5` support.

The center panel switches to a `Replay` heading and shows a transport bar between the canvas and the scrollable controls area:

- play/pause, restart, and stop icon buttons
- a Speed combo box (`0.25x`, `0.5x`, `1x`, `2x`, `4x`, `Max`)
- a full-width timeline slider for seeking within the recording
- current / total replay time

Enabled plugins continue to process replayed frames through the normal `PreviewFrame` path.

At EOF, replay shuts down its controller threads but stays in replay mode so the last frame, timeline, and transport controls remain available. Use `Restart`, drag the timeline, or click `Stop` to leave replay mode.

---

## Preview Contrast And Colormaps

The 2D preview uses a shared display model for the base image and ROI-grid overlay:

- `Auto` mode updates the display max from a combined histogram percentile (`90.0` to `100.0`)
- `Manual` mode lets you pin display min/max from the histogram window for repeatable inspection
- `Gamma` defaults to `0.5`, which keeps the previous square-root-like look while allowing flatter or steeper contrast curves
- `Event polarity` preserves the original ON=green / OFF=red rendering
- the other colormaps combine ON+OFF into one intensity channel for grayscale or false-color inspection
- fire / viridis / magma / inferno now use exact canonical LUT tables rather than coarse stop interpolation
- ruler, line-profile, and scale-bar overlays use outline/shadow rendering so they remain readable on bright colormaps

In 3D mode, the scroll area still switches over to point-cloud metrics instead of 2D display controls.

---

## External Tools

`Tools -> Stream to ImageJ...` opens a small connection dialog for the bundled Augur Bridge plugin
running inside ImageJ/Fiji.

- use `Save bundled plugin jar...` in the dialog to export `AugurBridge.jar`
- in ImageJ/Fiji, install that jar with `Plugins -> Install PlugIn...`, drag it onto the
  ImageJ/Fiji window, or copy it into the main `plugins/` folder
- do not place it in `plugins/Tools`, which is reserved for toolbar tools
- if ImageJ/Fiji does not refresh automatically, restart it or run `Help -> Refresh Menus`
- then run `Plugins -> Augur -> Start Bridge`
- the dialog includes the plugin-install/start steps, inline bridge status, and inline connection
  errors
- the default connection target is `127.0.0.1:57294`
- while the bridge is active, Augur shows a central placeholder instead of the local 2D preview and forwards the newest frame to ImageJ on a lossy background channel
- `Return to augur` disconnects the bridge and restores the in-app preview surface
- the connection is best-effort today: use it for live inspection, and verify the exact Fiji-side
  workflow manually on your machine

The repository includes both the installable jar and its source/build files under
`augur-gui/imagej-plugin/` in case you need to rebuild the plugin against a local `ij.jar`.

Histogram and line-profile tools also use deferred OS windows when the backend supports them, with embedded `egui::Window` fallbacks on backends that do not.

---

## Status Colors

Status, warning, and error labels adapt to the active GUI theme.

- warning and error text uses egui's theme-aware foreground colors, so replay notices, missing-dependency warnings, load errors, and runtime error messages stay readable in both light and dark mode
- replay and plugin success labels use a darker mid-green instead of a very bright light-green
- analysis info messages use a darker blue for better contrast on light backgrounds

---

## Runtime Notes

- Acquisition time is only adjustable for live preview and recording
- sensor geometry and disk-writer buffer are start-time controls, so they are only editable while idle
- EventStore budget and preview/point-cloud cadence update immediately from the `Settings` menu
- output path editing is disabled during active recording and replay
- the camera/replay status line below the toolbar shows the current session state
- the stats area also shows per-frame `ON % | OFF %` plus the event count for the latest preview frame
