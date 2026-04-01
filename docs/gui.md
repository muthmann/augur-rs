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
| `File` | Output path/browse, Open Replay, Save/Load Config, Close Replay, Export TIFF Stack during replay |
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
- `Acq time [ms]` in the top-bar `Settings` menu stays editable during replay and applies on the next replay frame boundary
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

- **ROI Grid** — built-in sensor partitioning and hotpixel-aware ROI selection
- **Runtime plugins** — any plugin loaded from `~/.augur/plugins/`, including scientific plugins from the [augur-plugins](https://github.com/muthmann/augur-plugins) repository

See the [Plugin Authoring Guide](./features/plugin-authoring-guide.md) for the plugin host model.

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

- the entire central viewer, including the heading strip, toolbar, canvas, replay transport, and
  lower control area, is rendered by one shared viewer component
- when the popup is open, that same viewer state moves into the popup host and the main window
  shows a placeholder plus a return button instead of running a second divergent renderer

### 2D preview tools

- hover the preview to read the current sensor-space `x, y` position together with a mode-aware pixel readout: most modes show ON / OFF / Total, while `Time Surface` shows the rendered decay `Value` plus `Total`
- the preview toolbar uses compact icon buttons with hover tooltips for ROI, line profile, ruler, annotation, histogram, zoom, and popup actions, and it stays on one row instead of wrapping the hover readout below the preview
- `Histogram` opens a mode-aware histogram plus brightness/contrast window with labeled intensity/count axes, hover readouts, Auto percentile, manual min/max, gamma controls, draggable marker handles, and a display-ramp preview
- the scroll area below the preview includes a `Mode` selector for red-blue polarity, signed count, time surface, and summed-intensity colormap rendering
- when `Time Surface` is active, a logarithmic `Decay τ [ms]` slider controls the temporal decay constant and Augur automatically requests raw preview events
- `Select ROI` turns the preview into a rectangular drag tool that writes back to `CameraConfig::roi`; during replay the disabled tooltip points users to rectangle annotations instead
- `Line Profile` samples ON and OFF intensity along a dragged line, opens after the drag completes, and includes labeled axes plus an optional ON+OFF sum trace
- `Ruler` measures dragged distances in both pixels and µm using the current pixel scale
- `Rect` and `Ellipse` add host-side software annotations; selecting one shows ROI statistics for ON, OFF, and combined channels
- the `Annotations` list is directly interactive: each row shows the ROI color, clicking a row selects it immediately, switches back to pointer/edit mode, and keeps visible ROI numbering contiguous after deletes
- in pointer mode, drag an existing rectangle or ellipse to move it
- `Esc` clears the active ROI/line/ruler/annotation draft, while `Delete` / `Backspace` removes the selected annotation
- zoom-out, zoom-in, and fit-to-window icon buttons control zoom
- when zoomed in, dragging the image pans the viewport
- `Crop to ROI` switches between full-frame and ROI-only rendering without changing the allocated canvas size; with a selected software annotation it crops to that ROI, otherwise it falls back to the hardware ROI, and ellipse crop uses the ellipse bounding box
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

The top-bar `Settings -> Acq time [ms]` control remains active during replay. Changing it updates
the replay preview accumulation window on the next frame boundary without restarting replay.

`File -> Export TIFF Stack…` opens a modal batch-export dialog during replay. It lets you choose a
time range, acquisition time, optional ROI crop, and output path, then writes one 16-bit grayscale
TIFF page per accumulation window.

At EOF, replay shuts down its controller threads but stays in replay mode so the last frame, timeline, and transport controls remain available. Use `Restart`, drag the timeline, or click `Stop` to leave replay mode.

---

## Preview Contrast And Colormaps

The 2D preview uses a shared display model for the base image and ROI-grid overlay:

- `Auto` mode updates the display max from the active mode's histogram percentile (`90.0` to `100.0`)
- `Manual` mode lets you pin display min/max from the histogram window for repeatable inspection
- `Gamma` defaults to `0.5`, which keeps the previous square-root-like look while allowing flatter or steeper contrast curves
- `Red-Blue Polarity` uses red for ON-dominant pixels, blue for OFF-dominant pixels, and magenta when both polarities balance at the same pixel
- `Signed Count` maps the net polarity (`ON - OFF`) through a blue-white-red diverging ramp while preserving a black background for pixels with no events
- `Time Surface` renders `exp(-(t_now - t_last) / τ)` in grayscale; if the current frame does not yet carry raw events, Augur falls back to grayscale summed intensity until a raw-event frame arrives
- the intensity modes combine ON+OFF into one channel for grayscale or false-color inspection
- fire / red hot / grays / green / cyan hot / magenta hot / ice / blue white red use the shared official LUT set
- changing the preview mode, time-surface decay, or histogram contrast controls immediately re-renders the current frame, including paused replay frames
- ruler, line-profile, and scale-bar overlays use outline/shadow rendering so they remain readable on bright colormaps

In 3D mode, the scroll area still switches over to point-cloud metrics instead of 2D display controls.

---

## External Tools

`Tools -> Stream to ImageJ...` opens a small connection dialog for the bundled Augur Bridge plugin
running inside ImageJ/Fiji.

- use `Save bundled plugin jar...` in the dialog to export `AugurBridge_.jar`
- in ImageJ/Fiji, install that jar with `Plugins -> Install PlugIn...`, drag it onto the
  ImageJ/Fiji window, or copy it into the main `plugins/` folder
- do not place it in `plugins/Tools`, which is reserved for toolbar tools
- if ImageJ/Fiji does not refresh automatically, restart it or run `Help -> Refresh Menus`
- then run `Plugins -> Augur -> Start Bridge`
- the plugin startup dialog lets you pick `Timeline (stack)` or `Live only (single frame)` and set a
  persisted `Max frames` cap for timeline mode
- the dialog includes the plugin-install/start steps, inline bridge status, and inline connection
  errors
- the default connection target is `127.0.0.1:57294`
- while the bridge is active, Augur shows a central placeholder instead of the local 2D preview and
  forwards frames to ImageJ on a bounded background queue, so short EDT stalls can absorb a brief
  burst without backpressuring the native preview/capture path
- in `Timeline (stack)` mode, ImageJ builds a bounded `ImageStack` with the normal slice slider,
  follows the newest frame while you stay on the live tail, stops auto-follow when you scrub away,
  resumes when you return to the last slice, and archives the current stack if the incoming frame
  dimensions change
- in `Live only (single frame)` mode, the plugin preserves the earlier single-window overwrite
  behavior
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

- acquisition time is adjustable during live preview, recording, and replay; replay applies changes on the next frame boundary
- sensor geometry and disk-writer buffer are start-time controls, so they are only editable while idle
- EventStore budget and preview/point-cloud cadence update immediately from the `Settings` menu, which now shows those cadences in Hz instead of milliseconds
- preview histogram work stays bounded even when a frame contains very large per-pixel counts, and
  time-surface mode reuses one cached decay pass for both the image and histogram views
- output path editing is disabled during active recording and replay
- the camera/replay status line below the toolbar shows the current session state
- the stats area also shows per-frame `ON % | OFF %` plus the event count for the latest preview frame
