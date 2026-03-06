# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with a live preview and runtime controls.

## Start the App

```bash
cargo run --bin augur-gui
```

---

## Toolbar

The top toolbar provides the main session controls:

| Button | Action |
|---|---|
| `Probe` | Query the EVK4 and display model, serial, and firmware information |
| `Preview` | Start live preview without writing to disk |
| `Record` | Start recording to the configured output path |
| `Stop` | Stop preview or recording |
| `Apply Settings` | Push pending runtime changes while previewing or recording |
| `Analysis` | Enable or disable plugins from a dropdown menu |
| `Save Config` / `Load Config` | Store or restore TOML settings |

---

## Settings Panel (Left)

Camera-only controls, always available regardless of plugins.

### Biases

Sliders for `diff_on`, `diff_off`, `fo`, `hpf`, `refr`. Each includes a short explanation of the tradeoff between sensitivity, latency, and noise.

### ROI

Set `x`, `y`, `width`, `height` to reduce the active sensor area and event bandwidth.

### Pixel Mask (DEM)

Manage the 64-slot hardware pixel mask:

- add coordinates manually
- remove coordinates from the list
- clear the list
- select a mask bitfield file

The GUI enforces the 64-slot hardware budget when adding pixels interactively.

### Digital Filters

Enable one of the two on-sensor filters: **STC** or **Trail**. They are mutually exclusive because they share the same hardware block.

---

## Analysis Panel (Right) — Plugin Extension Point

> **The analysis panel is powered by the plugin system.** The plugins shown here are loaded from the [augur-plugins](https://github.com/muthmann/augur-plugins) repository and compiled in. The panel only appears when at least one plugin is enabled.

Plugins run live alongside the preview stream. Each plugin gets a collapsible section in the panel with its own settings. Plugins can depend on each other — one plugin's output can feed the next within the same frame.

See the [Plugin Architecture](./features/analysis-plugins.md) doc for how to add your own.

### Plugins available in augur-plugins

**Hotpixel Detection**
- Highlights likely hotpixels in the preview
- Shows warnings in the main panel
- Can copy detected hotpixels directly into the DEM mask list

**ROI Grid**
- Computes hotpixel-free rectangular regions from the current mask
- Visualizes blocked and free regions on the preview
- Lists the largest valid rectangles; click `Use as ROI` to apply one

**Molecule Localization**
- Reconstructs a localization image from the event stream
- Denoises candidate spots with wavelet filtering
- Fits sub-pixel elliptical Gaussian emitters
- Renders crosshair markers at accepted positions
- Publishes localization results for downstream plugins

**Focus Metrics**
- Three selectable focus estimators:
  - Mean PSF sigma (from localization fits) — lower = sharper
  - FFT high-frequency power (standalone, no localization needed) — higher = sharper
  - Astigmatic `sigma_x / sigma_y` ratio
- Rolling history plot with coarse focus-quality indicator
- Dependency warnings surface when a localization-driven mode is active without the localization plugin

---

## Runtime Behavior

- In `Idle`: edits affect the next preview or recording session
- In `Previewing` or `Recording`: edits stay local until `Apply Settings`
- `Lock settings while recording` prevents runtime changes during capture
- Acquisition time can be adjusted separately from the main config and applied at runtime
