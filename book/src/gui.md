# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with live preview, file replay, runtime controls, and plugin-driven analysis.

## Start the App

```bash
cargo run --bin augur-gui
```

## Toolbar Workflow

The top toolbar provides the main session controls:

- `Probe Camera`: query the EVK4 and display model, serial, and firmware information
- `Preview`: start live preview without writing to disk
- `Record`: start recording to the configured output path
- `Open .raw`: open a recorded EVT3 file in replay mode
- `Stop`: stop preview, recording, or replay
- `Apply Settings`: push pending runtime changes while previewing or recording
- `Settings Panel` / `Analysis Panel`: show or hide the left and right side panels
- `Analysis`: enable or disable analysis plugins from a dropdown menu
- `Plugins`: open the plugin manager, rescan `~/.augur/plugins/`, or open the plugin directory
- `Save Config` / `Load Config`: store or restore TOML settings

The toolbar also shows the current session status and either the active replay file or the configured output path.

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
- a companion `<capture>.toml` sidecar is shown when available
- otherwise the GUI falls back to a geometry-matched default reference config

## Analysis Panel

When at least one analysis plugin is enabled, a right-side **Analysis Tools** panel appears.

- plugins keep running even if the panel is hidden
- disabling all plugins hides the panel automatically
- built-in ROI Grid settings still edit `CameraConfig` directly
- runtime-loaded plugins expose declarative settings and status entries through the FFI host
- plugins can exchange per-frame derived data through the shared context bus

Available plugin types include hotpixel detection, ROI grid analysis, molecule localization, and focus metrics.

## Plugin Manager

Use the toolbar **Plugins** menu to open the **Plugin Manager** window.

- the default plugin directory is `~/.augur/plugins/`
- each plugin lives in its own subdirectory with a `plugin.toml` manifest plus one `.dylib`, `.so`, or `.dll`
- `Scan for New Plugins` refreshes the loader without restarting the GUI
- `Reload` unloads and reloads one plugin, which is useful while iterating on plugin code
- load failures stay visible in the manager instead of crashing the app

## Replay Mode

Opening a `.raw` file starts a preview-only pipeline backed by the file replay camera in `augur-core`.

Replay adds a transport area below the preview with:

- `Play` / `Pause`
- `Restart`
- speed selection (`0.25x` to `Max`)
- a timeline slider for seeking
- current / total replay time
- MB progress

Enabled analysis plugins continue to process replayed frames through the same `PreviewFrame` path used for live preview.

When replay reaches EOF, the pipeline threads stop but the app stays in replay mode so the final frame and controls remain available for restart or seeking.

## Preview Contrast

A `Contrast` slider below the preview is available in both live and replay modes.

- it uses percentile-based normalization instead of raw max-value normalization
- this keeps single hotpixels from washing out the rest of the frame
- lower percentiles reveal dimmer activity sooner

## Runtime Notes

- acquisition time is only adjustable for live preview and recording
- output path editing is disabled during active recording and replay
- collapsing either side panel gives more space back to the preview
