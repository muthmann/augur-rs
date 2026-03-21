# Configuration Reference

AugurRS uses a TOML camera configuration with five top-level sections:

- `biases`
- `roi`
- `pixel_mask`
- `digital_filter`
- `global`

An example file is available at [examples/augur.toml](../examples/augur.toml).

## How Configuration Is Loaded

- `augur status` and `augur record` use built-in defaults unless `--config <path>` is provided
- `augur config show` prints the effective config
- `augur config set-bias`, `set-roi`, and `set-mask` update `augur.toml` by default unless `--config <path>` is provided
- `augur-gui` starts with defaults and can load or save TOML files from the toolbar

## Example

```toml
[biases]
diff_on = 0
diff_off = 0
fo = 0
hpf = 0
refr = 0

[roi]
x = 0
y = 0
width = 1280
height = 720

[pixel_mask]
masked_pixels = []

[digital_filter]
stc_enabled = false
stc_threshold_us = 1000
trail_enabled = false

[global]
nm_per_pixel = 65.0
sensor_width = 1280
sensor_height = 720
acq_time_ms = 50
event_store_budget_mib = 100
preview_interval_ms = 33
point_cloud_interval_ms = 67
disk_writer_buffer_mib = 4
```

## `biases`

Bias values are relative offsets from factory defaults.

- `diff_on`: ON-event contrast threshold
- `diff_off`: OFF-event contrast threshold
- `fo`: low-pass filter cutoff
- `hpf`: high-pass filter cutoff
- `refr`: refractory period tuning

The GUI exposes these as sliders with inline descriptions. The CLI exposes them through `augur config set-bias <key> <value>`.

Supported bias keys:

- `diff_on`
- `diff_off`
- `fo`
- `hpf`
- `refr`

## `roi`

The hardware ROI activates only a rectangular sensor region.

- Full sensor size is `1280 x 720`
- `width` and `height` must both be greater than zero
- `x + width` must stay within `1280`
- `y + height` must stay within `720`

Use ROI when you want to reduce event bandwidth and ignore unused parts of the image.

## `pixel_mask`

The IMX636 digital event mask suppresses individual defective pixels.

- `masked_pixels` is a list of `[x, y]` pairs
- `mask_file` can point to a precomputed bitfield file instead of explicit coordinates
- `hot_pixels` is accepted as a backward-compatible alias for `masked_pixels`

Operational notes:

- Coordinates must stay inside the `1280 x 720` sensor area
- The IMX636 hardware supports up to 64 masked pixels when programming the on-chip DEM slots
- The GUI includes tooling to copy detected hotpixels into the mask list

## `digital_filter`

The digital filter block configures on-sensor noise suppression.

- `stc_enabled`
- `stc_threshold_us`
- `trail_enabled`

Rules:

- STC and Trail are mutually exclusive
- For IMX636 runtime programming, `stc_threshold_us` should stay within `1000..=100000`

Use STC to suppress isolated noise bursts. Use Trail to keep the first event after a polarity transition and suppress redundant trailing events.

## `global`

The `global` section persists host-owned GUI/runtime settings.

- `nm_per_pixel`: shared pixel scale for plugins and downstream analysis
- `sensor_width`, `sensor_height`: configured sensor geometry reference used by the GUI and RAW header metadata
- `acq_time_ms`: acquisition window used for live preview/recording
- `event_store_budget_mib`: retained decoded-event history budget for runtime plugins
- `preview_interval_ms`: 2D preview repaint/update cadence
- `point_cloud_interval_ms`: 3D point-cloud repaint/update cadence
- `disk_writer_buffer_mib`: recording buffer size for the disk writer thread

Older TOML files without `[global]` still load with defaults.
