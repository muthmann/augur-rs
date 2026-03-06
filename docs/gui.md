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
- plugins can exchange per-frame derived data through a typed context
- hiding the panel does not disable plugin execution
- disabling all plugins hides the panel automatically

### Available plugin types

- Hotpixel detection
- ROI grid analysis
- Molecule localization
- Focus metrics

See [Plugin Architecture](./features/analysis-plugins.md) for the plugin host model.

---

## Replay Mode

Opening a `.raw` file starts a preview-only pipeline backed by `RawFileCamera`.

The center panel switches to a `Replay` heading and adds transport controls below the preview:

- `Play` / `Pause`
- `Step Forward` when paused
- replay speed selector (`0.25x`, `0.5x`, `1x`, `2x`, `4x`, `Max`)
- replay progress bar based on bytes consumed from the file

Enabled plugins continue to process replayed frames through the normal `PreviewFrame` path.

At EOF, replay stops cleanly and the app returns to idle automatically.

---

## Runtime Notes

- Acquisition time is only adjustable for live preview and recording
- output path editing is disabled during active recording and replay
- the camera/replay status line below the toolbar shows the current session state
