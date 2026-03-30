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
| `View` | Toggle Settings/Analysis panels, switch 2D/3D view mode |
| `Plugins` | Plugin Manager, Scan for New Plugins, Open Plugins Folder |
| `Analysis` | Per-plugin enable/disable checkboxes (shown only when plugins exist) |

The right side of the menu bar shows the `2D / 3D` view-mode toggle, status indicators (● REC, Finished), and the current camera/session status label.

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

- hover the preview to read the current sensor-space `x, y` cursor position
- `Select ROI` turns the preview into a rectangular drag tool that writes back to `CameraConfig::roi`
- `+`, `-`, and `Fit` control zoom
- when zoomed in, dragging the image pans the viewport
- `Crop to ROI` switches between full-frame and ROI-only rendering
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

- `▶` / `⏸` Play/Pause, `⏮` Restart, `⏹` Stop buttons
- a Speed combo box (`0.25x`, `0.5x`, `1x`, `2x`, `4x`, `Max`)
- a full-width timeline slider for seeking within the recording
- current / total replay time

Enabled plugins continue to process replayed frames through the normal `PreviewFrame` path.

At EOF, replay shuts down its controller threads but stays in replay mode so the last frame, timeline, and transport controls remain available. Use `Restart`, drag the timeline, or click `Stop` to leave replay mode.

---

## Preview Contrast

A `Contrast` slider below the preview is available in 2D mode only (live and replay). In 3D mode, point cloud metrics are shown instead.

- the slider controls percentile-based normalization (`90.0` to `100.0`)
- this prevents a single hotpixel from dominating the entire frame
- lower percentiles reveal dimmer activity at the cost of earlier saturation
- ON events render in green, OFF events render in red, and mixed pixels appear yellow/orange

The same contrast setting is used for the base preview image and ROI-grid rendering, with one shared normalization range across both polarity channels so relative ON/OFF strength remains visible.

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
