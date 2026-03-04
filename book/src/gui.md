# GUI Usage

The `augur-gui` desktop app wraps the same backend as the CLI with a live preview and runtime controls.

## Start The App

```bash
cargo run --bin augur-gui
```

## Toolbar Workflow

The top toolbar provides the main session controls:

- `Probe`: query the EVK4 and display model, serial, and firmware information
- `Preview`: start live preview without writing to disk
- `Record`: start recording to the configured output path
- `Stop`: stop preview or recording
- `Apply Settings`: push pending runtime changes while previewing or recording
- `Save Config` / `Load Config`: store or restore TOML settings

The GUI also shows the current output path and the current camera state.

## Settings Panel

The left panel groups runtime settings into focused sections.

### Biases

Sliders for:

- `diff_on`
- `diff_off`
- `fo`
- `hpf`
- `refr`

### ROI

Set:

- `x`
- `y`
- `width`
- `height`

Use ROI to reduce the active sensor area and event bandwidth.

### Pixel Mask (DEM)

Manage individual masked pixels in hardware:

- add coordinates manually
- remove coordinates from the list
- clear the list
- select a mask bitfield file

### Digital Filters

Enable one of the two on-sensor filters:

- STC
- Trail

They are mutually exclusive in the UI because they share the same hardware block.

## Runtime Behavior

- In `Idle`, edits affect the next preview or recording
- In `Previewing` or `Recording`, edits stay local until `Apply Settings`
- `Lock settings while recording` prevents runtime changes during capture
- Acquisition time can be adjusted separately from the main config and applied at runtime

## Analysis Tools

The GUI includes preview-side analysis utilities that do not change hardware state until you explicitly copy or apply settings.

### Hotpixel Analysis

- highlights likely hotpixels in the preview pipeline
- shows warnings in the main panel
- can copy detected hotpixels into the DEM mask list

### ROI Grid

The ROI-grid panel computes hotpixel-free rectangular regions from the current mask list.

Use it to:

- visualize blocked and free regions on the preview
- inspect the largest valid rectangles
- copy one candidate directly into the ROI fields with `Use as ROI`
