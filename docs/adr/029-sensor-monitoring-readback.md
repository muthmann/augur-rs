# ADR 029: Sensor Monitoring Readback For Absolute Setting Values

## Status

Accepted

## Context

`CameraConfig` expresses the five IMX636 biases as signed offsets around a
factory trim that differs per sensor unit. Operators tuning a bias in the GUI
therefore see a number with no physical meaning: `refr = -20` says nothing about
how long a pixel is actually blind, and the same offset produces a different
absolute code on a different camera. That makes a bias setting hard to reproduce
across units and impossible to report in a thesis in physical units.

Research against the Metavision SDK documentation and the OpenEB HAL found
exactly one settings-related absolute readback for this sensor:
`I_Monitoring::get_pixel_dead_time()`, which measures the refractory period in
µs from the `refractory_ctrl` register. The same monitoring block also reports
illumination in lux and die temperature in °C. For `diff_on`, `diff_off`, `fo`
and `hpf` there is **no** vendor-documented conversion to a physical unit and no
hardware readback; the datasheet that would contain one is under NDA. Third-party
characterisation papers publish fitted curves, but presenting a fit as a
measured value in a measurement tool would misrepresent its provenance.

This repository does not link the Metavision SDK — `augur-prophesee` is its own
Treuzell/libusb driver — so the readback has to be implemented against the
register map rather than called.

Two structural constraints shaped the design. First, register access is a
control-endpoint operation, and ADR 023 established that camera control runs on
a dedicated thread precisely because control transfers must never pause stream
reads. Second, the readback costs USB traffic on every poll, so it must not run
when nothing is displaying it.

## Decision

- **`augur-core::camera` owns the vocabulary.** `SensorMonitoring` carries
  `Option` fields for pixel dead time (µs), illumination (lux), temperature (°C)
  and a `BiasReadback` (`current` and `factory_default` `BiasCodes`). `None`
  means "this sensor cannot report it" — the host never substitutes a computed
  estimate for a missing measurement.
- **`EventCamera::read_monitoring()` is an optional trait method** defaulting to
  an empty reading, mirrored by `PseeSensor::read_monitoring` for per-sensor
  implementations. Replay, decoded imports and ingress sources inherit the
  default and report nothing.
- **Only the four abstract biases without a physical unit fall back to their
  absolute 8-bit code.** `factory_default + offset`, clamped to `0..=255`, is
  what OpenEB's `Imx636_LL_Biases` programs and what this driver's `encode_bias`
  already computed; surfacing it and the per-unit factory default is the
  honest absolute value.
- **The camera-control thread polls, behind a demand flag.**
  `PipelineController::sensor_monitoring_needed` gates the poll, which runs from
  the control thread's existing 50 ms idle tick at most every 500 ms. The GUI
  sets the flag from `settings_panel_open`, following the `raw_events_needed`
  precedent. A successful reconfigure resets the interval so post-apply codes
  appear immediately.
- **Sources whose camera runs inline on the stream thread get no monitoring at
  all.** Polling there would pause packet reads and cost recorded events, which
  ranks below any display concern.
- **Freshness is part of the contract.** `SensorMonitoringSnapshot` carries an
  age and the last error. Values shown beside a slider are withdrawn once the
  reading passes 2 s; a failed poll keeps the last good values but does not
  refresh the timestamp, so the age keeps growing. Clearing the demand flag
  discards the reading.

## Consequences

- The GUI shows the measured refractory period in µs beside the `refr` offset,
  and the absolute bias code beside each of the five biases. `augur status`
  prints the same values for lab logging.
- Illumination and die temperature become available. They are not settings, so
  they live in their own Sensor Readout section rather than among the biases —
  their value is as recorded context for a bias sweep.
- The plugin ABI is untouched. This is host-side telemetry; no plugin-visible
  type changes, no vtable change.
- Adding a new sensor requires no work: the default `read_monitoring` reports
  nothing and the panel degrades to the current behaviour.
- The IMX636 temperature ADC is now brought up (lazily, on first read), which
  OpenEB does during device init. The dead-time monitor's enable bits stay set
  after a read, as they do in OpenEB.
- Should Prophesee publish a bias-to-physical mapping, or the datasheet become
  available, the remaining four biases can be filled in behind the same
  `Option` fields without touching the transport, pipeline or UI structure.

## Alternatives considered

- **Compute physical values from published characterisation fits.** Rejected:
  the tool would show a fitted estimate in the same place, and the same
  typography, as a hardware measurement.
- **Request/response channel instead of a polled snapshot.** Rejected as more
  machinery than the problem needs; the shared-snapshot-plus-demand-flag shape
  already exists in the pipeline for `raw_events_needed` and pipeline stats.
- **Poll unconditionally while streaming.** Rejected: control transfers for a
  panel nobody has open are pure waste, and the demand flag is one atomic.

## References

- ADR 023 (split control and stream transport threads) — why the poll belongs on
  the control thread and why inline-camera sources are excluded.
- `docs/features/absolute-setting-values.md` — operator-facing behaviour, the
  full per-setting table, and register/formula provenance.
