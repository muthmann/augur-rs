# Absolute setting values (sensor monitoring readback)

> Feature brief — ADR 029

## What it does

The camera settings in `CameraConfig` are **abstract**: the five IMX636 biases
are signed offsets around a per-unit factory trim, not physical quantities. This
feature reads the sensor's own monitoring block and shows the absolute values
reported by that block. They are useful operating-context telemetry, but they
are not all calibrated measurements with known uncertainty.

- **Refractory period in µs** — estimated by the sensor, shown under the `refr`
  slider as `dead time  6.35 µs`. This is the equivalent of Metavision's
  `I_Monitoring::get_pixel_dead_time()`.
- **Absolute bias codes** — under every bias slider, `abs 118 · factory 138`:
  the 8-bit code currently programmed and this unit's factory default (the code
  for offset 0). Factory trim differs per camera, so the codes are information
  the offsets alone cannot express.
- **Sensor Readout section** — illumination in lux and die temperature in °C,
  the two remaining monitoring quantities. Neither is a setting; both describe
  the conditions under which a bias setting was used. They improve the
  provenance of a bias sweep, but do not by themselves make it reproducible.
  The section is always present and open by default: it states what it is
  waiting for when no camera is running rather than vanishing, so the readings
  are discoverable before the hardware is plugged in.

  Presentation: readings are two-column rows (`theme::inspector_row`) with the
  value right-aligned, rather than the space-padded monospace strings they used
  to be — values line up whatever their width, and stay lined up if a label or
  unit changes length. Hovering a row explains what the sensor measured. With
  no camera running the section is a single line; it no longer spends two grey
  paragraphs restating that these are live values while showing none.
- **Viewer diagnostics footer** — illumination and die temperature also appear
  on the viewer's one-line footer while streaming, so the conditions are
  readable without opening the settings sidebar. That copy is dropped once the
  readback is stale or errored; see
  [`viewer-toolbar-and-status-layout.md`](viewer-toolbar-and-status-layout.md).
- **`augur status`** prints the same readback (refractory period, illumination,
  die temperature, bias codes) for logging outside the GUI.
- **Plugins** read the same values through
  `HostContext::sensor_monitoring() -> Option<SensorMonitoringV1>`, delivered on
  the per-frame context bus under `CTX_SENSOR_MONITORING`.

## What has an absolute unit, and what does not

Researched against the Metavision SDK docs and the OpenEB HAL sources
(`TzImx636`, `Imx636_LL_Biases`, `Imx636RegisterMap`):

| Setting | Absolute value | Source |
|---------|----------------|--------|
| `refr` | Refractory period [µs] | Sensor estimate: `refractory_ctrl` (0x0020), `refr_counter / 200` |
| `diff_on`, `diff_off`, `fo`, `hpf` | **None** — bias code only | `bias_*` registers, `factory_default + offset` clamped to `0..=255` |
| ROI, pixel mask, STC/Trail threshold | Already absolute (px, µs) | Configuration is the unit |
| — | Illumination [lux] | Sensor conversion: `lifo_status` (0x0010) |
| — | Die temperature [°C] | Sensor conversion: `adc_status` (0x0050), `0.190 · code − 56` |

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

## Correctness and trust boundary

The implementation was audited against OpenEB 5.2.0's `TzImx636` and
`Imx636RegisterMap`. The register addresses, field masks, valid bits, retry
counts, and conversion formulas match that reference. Sony's public IMX636
flyer also confirms that the chip contains a thermometer, an absolute/relative
illuminometer, and a dead-time adjustment function.

That proves **formula and register parity**, not measurement accuracy. The
public documents give no accuracy, calibration uncertainty, spectral response,
field of view, response time, or unit-to-unit tolerance for the thermometer or
illuminometer. Metavision calls `get_pixel_dead_time()` an *estimated* dead
time. This audit did not include an EVK4 comparison against traceable
lux/temperature/time references, so the independent Rust implementation is not
yet validated against hardware or an external standard.

| Quantity | Calculation in Augur | Confidence | Appropriate use |
|----------|----------------------|------------|-----------------|
| Pixel dead time | enable `refr_en` + `refr_cnt_en`; wait for `refr_valid`; `(refr_counter & 0x0fff_ffff) / 200` µs | **High** formula parity; **medium** absolute accuracy because the vendor calls it an estimate and publishes no tolerance | Record the effective `refr` operating point; interpret event count, per-pixel rate and recovery behavior |
| Illumination | wait for `lifo_ton_valid`; `t = (counter & 0x07ff_ffff) / 100`; `lux = 10^(3.5 − log10(0.37·t))` | **High** formula parity; **low-to-medium** absolute metrology because calibration, spectrum, optics and spatial meaning are undocumented | Detect gross light-level changes and annotate operating conditions; use a calibrated photometer/photodiode for quantitative irradiance or fast modulation |
| Die temperature | start the on-chip temperature ADC; wait for `adc_done_dyn`; `°C = 0.190·(code & 0x03ff) − 56` | **High** formula parity; **medium** trend confidence and unknown absolute accuracy because no tolerance is public | Warm-up/drift provenance and thermal sanity checks; not a substitute for a calibrated temperature measurement |

The displayed decimal places are conversion resolution, not demonstrated
accuracy. In particular, OpenEB's public `I_Monitoring` API returns integer
values, while Augur preserves the fractional result of the register arithmetic.
A display such as `6.35 µs` must therefore not be reported as ±0.01 µs accuracy.

The illumination number is the sensor's own optical monitor behind the mounted
optics. It is not necessarily the illuminance at the experimental target, the
irradiance at a separate photodiode, or a photopic lux-meter reading for every
light spectrum. Treat comparisons as strongest within the same camera, lens,
aperture, spectrum, and geometry.

An independent EVK4 characterization found that the monitor tracked a very
wide light range, but its value was about 0.72 of the calculated on-chip
illuminance in that study's specific lens/setup; the authors attributed the
difference to optical transmission. This supports its usefulness as a trend
and context channel, not a universal correction factor.

There is also an unresolved public-source mismatch at the low-light end:
`Imx636RegisterMap` declares `lifo_ton` as 29 bits, while OpenEB's getter (and
Augur for parity) masks only 27 bits. Without the non-public sensor application
note, the upper two bits cannot be classified confidently as intentional,
reserved, or a reference-driver limitation. Do not infer calibrated sub-lux
precision from the raw counter range.

## Acquisition and persistence

Live display without recording retains the bounded 500 ms (2 Hz) poll. The
opt-in companion recorder instead performs field-selective reads: illumination
at 1.5 Hz, temperature/dead time at 0.2 Hz, and bias codes at start/after an
apply. OpenEB's official HAL showcase queries monitoring at most once per
second, so 1.5 Hz remains an engineering choice rather than a proven monitor
bandwidth. It must be validated by the hardware A/B check below before a
zero-interference claim.

| Quantity | Recommended recording cadence | Rationale |
|----------|-------------------------------|-----------|
| Pixel dead time and bias codes | once after camera start and after every accepted bias change; optionally repeat at 0.2–1 Hz as a health check | Dead time is primarily a function of the programmed `refr` operating point. Event-driven samples preserve each configuration transition without duplicating a nominally static value on every frame. |
| Die temperature | 0.2–1 Hz (every 1–5 s); use 1 Hz during warm-up or thermal experiments | Die temperature normally changes much more slowly than event data. A low-rate series captures warm-up and drift without pretending to be frame-synchronous. |
| Illumination | 1 Hz by default, at most the current 2 Hz for contextual logging; sample with a synchronized external photodiode for fast or quantitative optical experiments | The public LIFO timing/response specification is absent. Faster register polling does not turn this monitor into a high-bandwidth optical channel and can alias flicker or modulation. |

If these values are persisted, store a timestamped telemetry series or
configuration-change records, not a copy on every event frame. Each record
should include the camera serial, current bias codes/configuration, acquisition
timestamp (or an explicit host monotonic timestamp), and quality state
(`valid`, `stale`, or read error). The current `age_s` is essential because a
monitoring sample is attached to a later preview/plugin frame and is never
simultaneous with it.

The values remain outside the raw EVT3 stream. When **Record sensor monitoring**
is enabled, they are persisted as `<raw-stem>.sensor-monitoring.csv`; otherwise
replay/offline analysis has no monitoring series. The CSV records monotonic
poll start/end times and RAW payload byteoffset brackets, not a fabricated
sensor timestamp. See `sensor-telemetry-recording.md` and ADR 031.

The current plugin payload is not yet a sufficient durable telemetry record:
it has `age_s`, but no sample ID, absolute/monotonic acquisition timestamp,
quality/error field, or per-field timestamp. The same cached sample is
serialized onto every processed frame, so a logger must not mistake repeated
payloads for new sensor acquisitions. The sensor reads are sequential (dead
time, lux, temperature, then bias codes), and the snapshot time is taken only
after the whole poll; the fields are therefore not exactly simultaneous.

## Interaction with event measurements

The quantities are not independent of the event stream:

- **Illumination is a major operating condition.** Sony specifies different
  latency and background-rate limits at 1000 lux and 5 lux. A changing lux
  value can therefore explain changes in latency, background activity, event
  rate, ON/OFF balance, or usable bias range; it is not itself evidence that the
  monitor disturbed those measurements.
- **Dead time directly changes the data.** After an event, the pixel is blind
  for the refractory period. It limits repeated events from one pixel, changes
  event counts during large transitions, and can degrade recovery or waveform
  reconstruction for rapid stimuli. It must be held fixed or logged for
  event-rate and timing comparisons.
- **Temperature is a potential confounder, not a correction factor.** Analog
  pixel behavior and noise can be temperature-dependent, but no public IMX636
  transfer function maps the reported die temperature to a bias, threshold, or
  event correction. Log it and characterize correlations experimentally; do
  not automatically compensate event data from this value.

Reading the monitors also changes a small amount of sensor state:

- IMX636 initialization enables the IPH mirror and LIFO counter, exactly as the
  OpenEB constructor does; these stay enabled even when the GUI panel is closed.
- The first temperature read enables the temperature buffer/ADC; each sample
  normally gates the ADC clock on for one conversion and off afterwards. A
  transport error during the start/status sequence currently returns before
  the cleanup write, so clock-off is not guaranteed on every error path.
- The first dead-time read enables the refractory monitoring counter and leaves
  it enabled, matching OpenEB. This is register `0x0020`, separate from the
  `bias_refr` DAC at `0x1020`, so polling does not rewrite the configured
  refractory bias.

Control commands and stream reads use different USB endpoints and different
threads, with multiple stream transfers kept queued. Monitoring therefore does
not synchronously pause the raw stream in the supported EVK4 path. It does,
however, share the device and physical USB link, and one full poll performs
roughly 18 register transactions in a normal successful steady-state read and
more when valid-bit retries are needed. Each Treuzell transaction is a small
bulk-OUT/bulk-IN pair. There is no hardware stress-test yet proving zero effect
at maximum event/link load or proving that the enabled analog monitor blocks
are electrically invisible. For measurement-critical operation, compare
identical raw captures with monitoring demand off/on and check byte continuity,
dropped-buffer counters, event counts, polarity balance, noise, die
temperature, and timing before claiming non-interference.

A transport failure in any later field currently retains the entire previous
snapshot, even if dead time or lux had already been read successfully in that
poll. The GUI keeps those stale sensor-panel values visible with an age/error
warning; only the values placed directly beside bias sliders are hidden after
2 s. Plugins receive the stale value and age but not the error. Consumers must
therefore enforce their own age limit.

## How it works

1. **`augur-core::camera`** defines `SensorMonitoring` (all fields `Option`),
   `BiasReadback`, and `BiasCodes`, plus
   `EventCamera::read_monitoring() -> Result<SensorMonitoring>`, defaulting to an
   empty reading. The additive `read_monitoring_selected` hook enables physical
   field-specific polling for the companion recorder.
2. **`Imx636::read_monitoring`** performs the register sequences ported from
   OpenEB's `TzImx636`. The temperature ADC bring-up (`temperature_init` in
   OpenEB) is deferred to the first temperature read, so a session that never
   requests monitoring avoids that ADC setup. The LIFO/IPH monitor is already
   enabled during sensor initialization, as in OpenEB.
3. **The control thread polls it.** Camera control already runs on its own thread
   (ADR 023); the poll is issued from that thread's 50 ms idle tick, at most
   every `SENSOR_MONITORING_INTERVAL` (500 ms), and only while
   `PipelineController::sensor_monitoring_needed` is set. Applying settings
   resets the interval so a new read is attempted immediately; the timer is
   currently reset even if reconfiguration failed.
4. **The GUI uses coarse demand gating.** `sync_pipeline_requirements` sets the
   flag while the settings panel is open or any runtime plugin is enabled. A
   collapsed panel with no enabled plugins costs zero polling transfers, but
   plugins cannot yet declare whether they actually consume this context.

## Plugin access

`HostContext::sensor_monitoring()` returns `Option<SensorMonitoringV1>` for the
current frame. The host serializes the snapshot into the per-frame context bus
under `CTX_SENSOR_MONITORING` (`"augur.sensor_monitoring"`), the same mechanism
`GlobalSettings` uses — **no vtable or ABI change**, so existing plugins keep
working without a rebuild.

Design notes:

- **Context bus, not the frame ABI.** A typed frame field would have been
  `None` in exactly the deterministic offline path that ADR 025 makes the
  primary analysis workflow, and would have forced every plugin to be rebuilt
  for a value most do not use.
- **Optional context, never an input to results.** Live capture has it, replay
  and offline runs do not. A plugin whose results depend on it would disagree
  between a live preview and an offline re-run of the same recording. The type
  documentation and the authoring guide both say so explicitly.
- **The poll is demand-driven from either consumer.** The GUI sets
  `sensor_monitoring_needed` when the settings panel is open **or any** runtime
  plugin is enabled, so a plugin gets readings with the panel collapsed. The
  current capability model cannot tell whether an enabled plugin actually
  consumes monitoring, so this is coarse demand gating rather than per-plugin
  subscription.
- **Not deduplicated.** Unlike `GlobalSettings`, the latest snapshot is
  serialized for every processed frame, even when the sensor has not been
  polled again. A logger needs a future sample ID/timestamp or must deduplicate
  using value and age behavior.
- **`age_s` travels with the value**, because the host polls at a few hertz and
  a reading is never simultaneous with the frame it arrives on.

## Guarantees and limitations

- **Never on the stream thread.** Sources without
  `split_stream_reader` (replay, decoded imports, Python ingress, tests) run
  camera control inline on the packet-read thread, where register access would
  pause reads and cost recorded events. Those sources report no monitoring at
  all — `PipelineController::sensor_monitoring()` returns `None`.
- **An all-`None` reading is published as `None`.** The current model therefore
  cannot distinguish "unsupported sensor" from "all monitor fields temporarily
  invalid" without a transport error.
- **Staleness is visible but consumer-specific.** Past
  `MONITORING_STALE_AFTER_S` (2 s) the per-slider absolute values disappear.
  The Sensor Readout section keeps the last values visible with an age/error
  warning, and plugins receive the age but no error. A failed poll leaves the
  timestamp unchanged so the age keeps growing.
- **Closing the panel drops the reading**, so a reopened panel never shows
  values from before it was closed as if they were live.
- **The dead-time monitor stays enabled.** `refr_en`/`refr_cnt_en` are left set
  after a read, as OpenEB leaves them. `refractory_ctrl` (0x0020) is a
  monitoring register, distinct from the `bias_refr` DAC (0x1020), so this does
  not alter pixel configuration.
- **Illumination is reported only for a usable integration window.** An empty or
  saturated LIFO counter yields no lux value rather than an infinity. The
  counter is masked to 27 bits to match OpenEB's extraction. The public register
  map declares a 29-bit field, so the effect of the dropped upper bits at very
  low illumination remains an explicit reliability gap.
- **No public calibration claim is added by Augur.** `lux`, `°C`, and `µs`
  identify the vendor conversion's output units, not traceable calibration or
  guaranteed accuracy.
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
- `augur-gui/src/viewer_widget.rs` — `sensor_footer_readings`, the freshness
  filter behind the footer copy of the readings
- `augur-gui/src/app.rs` — demand flag in `sync_pipeline_requirements`
- `augur-cli/src/main.rs` — `augur status` readback block

## References

- Metavision SDK, [Biases](https://docs.prophesee.ai/stable/hw/manuals/biases.html)
  — IMX636 biases are offsets; points to `get_pixel_dead_time()` for the µs
  mapping.
- Metavision SDK, [HAL Facilities](https://docs.prophesee.ai/stable/api/cpp/hal/facilities.html)
  — `I_Monitoring::get_pixel_dead_time` / `get_illumination` /
  `get_temperature`.
- Sony Semiconductor Solutions,
  [IMX636-AAMR-C public flyer](https://www.sony-semicon.com/files/62/flyer_industry/IMX636-AAMR-Flyer.pdf)
  — public feature existence and illumination-dependent latency/background-rate
  characteristics; no monitor accuracy specification.
- OpenEB 5.2.0:
  [`imx636_tz_device.cpp`](https://github.com/prophesee-ai/openeb/blob/5.2.0/hal_psee_plugins/src/devices/imx636/imx636_tz_device.cpp),
  [`imx636_registermap.h`](https://github.com/prophesee-ai/openeb/blob/5.2.0/hal_psee_plugins/include/devices/imx636/register_maps/imx636_registermap.h),
  `imx636_ll_biases.cpp`, and the
  [`metavision_hal_showcase` 1 Hz polling example](https://github.com/prophesee-ai/openeb/blob/5.2.0/hal/cpp/samples/metavision_hal_showcase/metavision_hal_showcase.cpp#L337-L365).
- Lopes et al.,
  [Characterization of Event-Based Vision Sensors for High-Speed Optical Instrumentation](https://arxiv.org/abs/2607.04741)
  — IMX636 response depends on illumination, refractory behavior, spatial
  activity and readout serialization; timestamp resolution alone is not
  waveform fidelity.
- McMahon-Crabtree et al.,
  [Initial Characterization of the Sony IMX636 Event-Based Vision Sensor](https://doi.org/10.1117/12.3026253)
  — independent illuminometer range/optical-path characterization in one EVK4
  setup; not a universal calibration.
