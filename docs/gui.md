# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with live preview, file replay, runtime controls, and plugin-driven analysis.

## Start the App

```bash
cargo run --bin augur-gui
```

---

## Toolbar

The top toolbar provides the main session controls:

| Button | Action |
|---|---|
| `Probe Camera` | Query the EVK4 and display model, serial, and firmware information |
| `Preview` | Start live preview without writing to disk |
| `Record` | Start recording to the configured output path |
| `Open .raw` | Open a recorded EVT3 file in replay mode |
| `Stop` | Stop preview, recording, or replay |
| `Apply Settings` | Push pending runtime changes while previewing or recording |
| `Settings Panel` / `Analysis Panel` | Show or hide the left and right side panels |
| `Analysis` | Enable or disable plugins from a dropdown menu |
| `Plugins` | Open the plugin manager, rescan `~/.augur/plugins/`, or open the plugin directory |
| `Save Config` / `Load Config` | Store or restore TOML settings |

---

## Settings Panel (Left)

Camera-only controls are shown in the left panel.

### Live camera sessions

- In `Idle`, edits affect the next preview or recording session
- In `Previewing` or `Recording`, edits stay local until `Apply Settings`
- `Lock settings while recording` prevents runtime changes during capture

### Replay sessions

- the panel becomes read-only
- if a companion `<capture>.toml` sidecar exists, it is shown as reference data
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

## Replay Mode

Opening a `.raw` file starts a preview-only pipeline backed by `RawFileCamera`.

The center panel switches to a `Replay` heading and adds transport controls below the preview:

- `Play` / `Pause`
- `Restart`
- replay speed selector (`0.25x`, `0.5x`, `1x`, `2x`, `4x`, `Max`)
- a timeline slider for seeking within the recording
- current / total replay time
- replay MB progress as secondary info

Enabled plugins continue to process replayed frames through the normal `PreviewFrame` path.

At EOF, replay shuts down its controller threads but stays in replay mode so the last frame, timeline, and transport controls remain available. Use `Restart`, drag the timeline, or click `Stop` to leave replay mode.

---

## Preview Contrast

A `Contrast` slider below the preview is available in both live and replay modes.

- the slider controls percentile-based normalization (`90.0` to `100.0`)
- this prevents a single hotpixel from dominating the entire frame
- lower percentiles reveal dimmer activity at the cost of earlier saturation

The same contrast setting is used for the base preview image and ROI-grid rendering.

---

## Runtime Notes

- Acquisition time is only adjustable for live preview and recording
- output path editing is disabled during active recording and replay
- the camera/replay status line below the toolbar shows the current session state
