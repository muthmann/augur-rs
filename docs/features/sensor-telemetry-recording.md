# Sensor telemetry companion recording

> Feature brief — ADR 031

## What it does

Live IMX636 monitoring can be recorded alongside a RAW event stream when the
operator enables **Record sensor monitoring** in the Capture card. The option is
off by default and is locked for the duration of a recording.

The option is part of the persisted global camera configuration and named
camera profiles. Loading or plugin-applying a profile therefore restores the
actual recording behavior, not only the visible checkbox. Older configuration
files omit the field and keep the safe default `false`.

For a RAW target such as:

```text
experiment_20260730_143012.raw
```

Augur creates:

```text
experiment_20260730_143012.sensor-monitoring.csv
```

The recording sidecar names the companion file, rates, clock semantics, written
and dropped row counts, read errors, and any writer error.

## Sampling profile

The rates are deliberately fixed rather than exposed as another set of
acquisition controls:

| Fields | Rate | Physical sensor access |
|---|---:|---|
| Illumination | 1.5 Hz (every 666.667 ms) | LIFO block only |
| Die temperature and pixel dead time | 0.2 Hz (every 5 s) | temperature ADC and refractory monitor only |
| Absolute bias codes | recording start and after a settings apply | bias registers only |

Missed deadlines are skipped; the scheduler never emits a catch-up burst. The
IMX636 implementation overrides the additive
`read_monitoring_selected(selection)` camera hook, so slow fields are not read
at the lux rate merely to discard them.

## CSV v1

Each row represents one actual control-thread register poll:

```text
schema_version,sample_id,poll_kind,host_elapsed_start_us,host_elapsed_end_us,
raw_data_offset_before_bytes,raw_data_offset_after_bytes,illumination_lux,
temperature_c,pixel_dead_time_us,bias_diff_on_code,bias_diff_off_code,
bias_fo_code,bias_hpf_code,bias_refr_code,status,error
```

- `host_elapsed_*` is monotonic time relative to pipeline creation, not UTC.
- `raw_data_offset_*` is the cumulative binary EVT3 payload accepted by the
  stream thread before and after the register read. It excludes the text header.
- Empty value fields mean that field was not due in that poll.
- `status` is `valid`, `unsupported`, or `error`. Errors never reuse an old
  value as a new sample.

The two brackets preserve what is actually known. Monitoring registers do not
return a sensor timestamp, and dead time, lux, temperature, and biases are read
sequentially. The RAW EVT3 stream uses a relative sensor clock with a nominal
1-µs tick, while the CSV uses host monotonic time. Therefore the CSV does **not**
claim exact simultaneity with an event.

Offline tooling can decode the RAW stream around the recorded byte offsets and
associate a row with the surrounding event-time interval. That remains a
host/transport correlation: USB, firmware buffering, and register-read latency
are not publicly bounded. A timing-critical experiment needs a hardware
`EXT_TRIGGER` or a photodiode/modulation marker in the RAW stream; those edges
share the event clock.

## Event-stream priority and failure policy

Monitoring remains on the camera-control thread, separate from the stream
reader. A bounded telemetry queue feeds its own buffered writer:

- the control thread uses `try_send` and never waits for the CSV writer;
- a full/disconnected queue increments `samples_dropped`;
- monitoring read and CSV write failures are recorded as telemetry quality
  failures and do not stop or downgrade the RAW recording;
- enabling telemetry fails recording startup if the companion file already
  exists or cannot be created;
- cameras without an independent control thread reject telemetry recording,
  because register access on an inline stream thread could lose events.

This design prevents the CSV path from applying backpressure to the RAW disk
queue. It does not prove that USB control traffic or enabled on-chip monitor
blocks are electrically invisible. Before using 1.5-Hz lux monitoring in a
measurement-critical high-rate setup, perform a hardware A/B capture with the
option off/on and compare RAW byte/event counts, reader-stall metrics, noise,
polarity balance, and any device overflow counter that becomes available.

## Capture UX

The left Camera Settings panel now starts with a fixed Capture card containing:

- camera state and direct Probe/Preview/Record/Stop actions;
- the RAW output target and file chooser;
- the opt-in telemetry checkbox and companion filename/rates;
- telemetry written/dropped counts while recording;
- Apply Settings only when live settings are dirty.

The central idle state also offers Probe Camera/Open Replay actions, followed by
Preview/Record once a camera has been detected. The Camera/File menus remain
available as secondary access paths.

## Files

- `augur-core/src/camera.rs` — selective monitoring request model
- `augur-core/src/pipeline.rs` — scheduler, CSV writer, failure counters, RAW
  byteoffset brackets, companion-path helper
- `augur-prophesee/src/sensors/imx636.rs` — physical field-selective reads
- `augur-gui/src/app.rs` — opt-in state and Capture card
- `augur-gui/src/viewer_widget.rs` — central idle call-to-action
- `docs/adr/031-sensor-telemetry-companion-recording.md` — architectural
  rationale and time-model decision
