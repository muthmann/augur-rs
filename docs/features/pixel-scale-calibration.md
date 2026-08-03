# Pixel Scale Calibration

## Summary

Every physical length AugurRS prints — the scale bar, the ruler readout, the
line-profile length note — is `pixel_count × nm_per_pixel`. The default
`nm_per_pixel` is the bare IMX636 sensor pitch (4860 nm), which is the
sample-plane scale only when the sensor looks directly at the sample. Behind a
microscope, a lens or any relay it is wrong by the magnification.

AugurRS therefore tracks *whether anyone has confirmed the scale for this
setup* alongside the number itself, and marks every derived micrometre reading
until they have. See ADR 033 for the reasoning.

## Behaviour

- `Settings ▸ Pixel scale (nm/px)…` reads
  **`Pixel scale (nm/px) — uncalibrated…`** until the calibration is confirmed.
- The submenu carries the value plus a **`Calibrated for this setup`**
  checkbox. Ticking it is a statement, not a computation: the program has no way
  to discover the magnification.
- While uncalibrated:
  - the scale bar renders `200 µm (uncal.)`
  - the ruler readout renders `12.3 px · 59.78 µm (uncal.)`, both on the canvas
    and in the viewer footer, with a hover explanation
  - the line-profile window's length note carries the same suffix and hover text
- Pixel counts are never marked — they are exact regardless of calibration.

## Calibrating

1. Determine the effective sample-plane pixel size: `sensor pitch ÷ total
   optical magnification`. For an IMX636 behind a 20× objective with a 1× tube
   lens that is `4860 / 20 = 243` nm/px.
2. Enter it under `Settings ▸ Pixel scale (nm/px)…`.
3. Tick `Calibrated for this setup`.

For direct detection (no optics), the default is already correct — tick the box
without changing the value.

## Configuration

`GlobalSettingsConfig` in `augur.toml`:

```toml
[global]
nm_per_pixel = 243.0
pixel_scale_calibrated = true
```

`pixel_scale_calibrated` is `#[serde(default)]`, so a config file written before
this field existed loads as **uncalibrated**. That is intentional: a file that
never recorded the claim cannot make it.

## Implementation

| Item | Location |
| --- | --- |
| `PixelScale { nm_per_pixel, calibrated }` | `augur-gui/src/viewer_tools/scale_bar.rs` |
| `calibration_suffix`, `UNCALIBRATED_TOOLTIP` | `augur-gui/src/viewer_tools/scale_bar.rs` |
| `RulerMeasurement::label()` | `augur-gui/src/viewer_tools/ruler.rs` |
| Scale-bar suffix | `viewer_widget::paint_scale_bar` |
| Line-profile length note | `viewer_tools::line_profile::render_line_profile_viewport` |
| `pixel_scale_calibrated` config field | `augur-core/src/config.rs` |
| `CameraApp::pixel_scale()` | `augur-gui/src/app.rs` |

`PixelScale` is a single value carrying both the number and its provenance, so
no call site can format micrometres while forgetting the caveat.

## Scope

- Only the host's own measurement readouts are marked. `nm_per_pixel` is still
  handed to plugins and to the ImageJ bridge as a plain `f64`; a plugin that
  reports lengths is responsible for its own provenance.
- The flag does not change any number. It changes only what the UI claims about
  the number.

## Related

- ADR 033: Measurement Provenance In The UI
- `docs/features/viewer-tools-and-imagej.md`
- `docs/features/global-settings-menu.md`
- `docs/features/absolute-setting-values.md`
