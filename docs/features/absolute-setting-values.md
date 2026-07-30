# Absolute setting values (sensor monitoring readback)

> Feature brief — ADR 029

## What it does

The camera settings in `CameraConfig` are **abstract**: the five IMX636 biases
are signed offsets around a per-unit factory trim, not physical quantities. This
feature reads the sensor's own monitoring block and shows, next to each abstract
control, the absolute value it actually produced.

- **Refractory period in µs** — measured by the sensor, shown under the `refr`
  slider as `dead time  6.35 µs`. This is the equivalent of Metavision's
  `I_Monitoring::get_pixel_dead_time()`.
- **Absolute bias codes** — under every bias slider, `abs 118 · factory 138`:
  the 8-bit code currently programmed and this unit's factory default (the code
  for offset 0). Factory trim differs per camera, so the codes are information
  the offsets alone cannot express.
- **Sensor Readout section** — illumination in lux and die temperature in °C,
  the two remaining monitoring quantities. Neither is a setting; both describe
  the conditions a bias setting was chosen under, which is what makes a bias
  sweep reproducible. The section is always present and open by default: it
  states what it is waiting for when no camera is running rather than vanishing,
  so the readings are discoverable before the hardware is plugged in.
- **`augur status`** prints the same readback (refractory period, illumination,
  die temperature, bias codes) for logging outside the GUI.

## What has an absolute unit, and what does not

Researched against the Metavision SDK docs and the OpenEB HAL sources
(`TzImx636`, `Imx636_LL_Biases`, `Imx636RegisterMap`):

| Setting | Absolute value | Source |
|---------|----------------|--------|
| `refr` | Refractory period [µs] | Measured: `refractory_ctrl` (0x0020), `refr_counter / 200` |
| `diff_on`, `diff_off`, `fo`, `hpf` | **None** — bias code only | `bias_*` registers, `factory_default + offset` clamped to `0..=255` |
| ROI, pixel mask, STC/Trail threshold | Already absolute (px, µs) | Configuration is the unit |
| — | Illumination [lux] | Measured: `lifo_status` (0x0010) |
| — | Die temperature [°C] | Measured: `adc_status` (0x0050), `0.190 · code − 56` |

**There is no vendor-documented conversion from `diff_on`/`diff_off`/`fo`/`hpf`
to a contrast percentage or a cutoff frequency**, and the sensor has no readback
for them. Prophesee's own documentation points to `get_pixel_dead_time()` for
`refr` and to the (NDA) sensor datasheet for everything else. Fitted curves from
third-party characterisation papers are deliberately *not* used: a measurement
tool must not present an estimate in a physical unit as if the hardware had
reported it. The absolute bias code is shown instead — it is the real value the
sensor works with.

Metavision's other absolute-unit facilities (anti-flicker Hz, ERC events/s,
trigger-out period) drive hardware blocks this driver does not program, so they
have no counterpart here.

## How it works

1. **`augur-core::camera`** defines `SensorMonitoring` (all fields `Option`),
   `BiasReadback`, and `BiasCodes`, plus
   `EventCamera::read_monitoring() -> Result<SensorMonitoring>`, defaulting to an
   empty reading. `augur-prophesee::PseeSensor` has the matching per-sensor hook.
2. **`Imx636::read_monitoring`** performs the register sequences ported from
   OpenEB's `TzImx636`. The temperature ADC bring-up (`temperature_init` in
   OpenEB) is deferred to the first temperature read, so a session that never
   opens the settings panel pays nothing for it.
3. **The control thread polls it.** Camera control already runs on its own thread
   (ADR 023); the poll is issued from that thread's 50 ms idle tick, at most
   every `SENSOR_MONITORING_INTERVAL` (500 ms), and only while
   `PipelineController::sensor_monitoring_needed` is set. Applying settings
   resets the interval so the panel shows post-apply codes immediately.
4. **The GUI asks only when it can show it.** `sync_pipeline_requirements`
   stores `settings_panel_open` into the demand flag, following the existing
   `raw_events_needed` pattern. A collapsed panel costs zero control transfers.

## Guarantees and limitations

- **Never on the stream thread.** Sources without
  `split_stream_reader` (replay, decoded imports, Python ingress, tests) run
  camera control inline on the packet-read thread, where register access would
  pause reads and cost recorded events. Those sources report no monitoring at
  all — `PipelineController::sensor_monitoring()` returns `None`.
- **An all-`None` reading is published as `None`.** "This source has no
  monitoring block" and "all values are momentarily missing" must not look the
  same to the UI.
- **Stale readings are withdrawn, not shown.** Past
  `MONITORING_STALE_AFTER_S` (2 s) the per-slider absolute values disappear and
  the Sensor Readout section reports the age; a value sitting next to a slider
  is always current. A failed poll keeps the last good values visible, marks
  them with the error, and leaves the timestamp alone so the age keeps growing.
- **Closing the panel drops the reading**, so a reopened panel never shows
  values from before it was closed as if they were live.
- **The dead-time monitor stays enabled.** `refr_en`/`refr_cnt_en` are left set
  after a read, as OpenEB leaves them. `refractory_ctrl` (0x0020) is a
  monitoring register, distinct from the `bias_refr` DAC (0x1020), so this does
  not alter pixel configuration.
- **Illumination is reported only for a usable integration window.** An empty or
  saturated LIFO counter yields no lux value rather than an infinity. The
  counter is masked to 27 bits to match OpenEB's extraction; the two dropped
  bits only matter far below 0.01 lux.
- **`augur status` does not apply the config file.** The bias codes it prints
  are whatever the device currently holds — after a fresh open, the factory
  trim. The output labels this.

## Files

- `augur-core/src/camera.rs` — `SensorMonitoring`, `BiasReadback`, `BiasCodes`,
  `EventCamera::read_monitoring`
- `augur-core/src/pipeline.rs` — `SensorMonitoringSnapshot`,
  `poll_sensor_monitoring`, `PipelineController::sensor_monitoring`,
  `sensor_monitoring_needed`
- `augur-prophesee/src/sensors/imx636.rs` — register sequences and conversions
- `augur-prophesee/src/sensors/mod.rs`, `augur-prophesee/src/evk4.rs` — hooks
- `augur-gui/src/settings.rs` — per-slider readouts, Sensor Readout section
- `augur-gui/src/app.rs` — demand flag in `sync_pipeline_requirements`
- `augur-cli/src/main.rs` — `augur status` readback block

## References

- Metavision SDK, [Biases](https://docs.prophesee.ai/stable/hw/manuals/biases.html)
  — IMX636 biases are offsets; points to `get_pixel_dead_time()` for the µs
  mapping.
- Metavision SDK, [HAL Facilities](https://docs.prophesee.ai/stable/api/cpp/hal/facilities.html)
  — `I_Monitoring::get_pixel_dead_time` / `get_illumination` /
  `get_temperature`.
- OpenEB: `hal_psee_plugins/src/devices/imx636/imx636_tz_device.cpp`,
  `imx636_ll_biases.cpp`, `include/devices/imx636/register_maps/imx636_registermap.h`.
