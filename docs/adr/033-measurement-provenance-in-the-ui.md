# ADR 033: Measurement Provenance In The UI

## Status

Accepted

## Context

A walkthrough of the GUI found a class of defects that share one shape: a
surface stated something the program could not back up.

- The scale bar, ruler and line profile printed micrometres derived from
  `GlobalSettingsConfig::nm_per_pixel`, whose default is the bare IMX636 pixel
  pitch (4860 nm). Behind any optics that number is wrong by the magnification,
  yet the readout looked identical to a calibrated one. The scale bar is on by
  default, so a screenshot taken five minutes after first launch carried an
  authoritative-looking µm bar that nobody had ever calibrated.
- The preview histogram labelled its x axis `Pixel Intensity` and the line
  profile labelled its y axis `Intensity`. Both quantities are event counts,
  and in time-surface mode the histogram bins are decay values. Naming them
  "intensity" invites reading an event camera as a photometer.
- The line profile plotted the Bresenham sample index as if it were a distance.
  On a 45° line the two differ by √2, so a feature's measured width was off by
  41% with nothing on screen to say so.
- The ImageJ bridge dropped preview frames whenever its bounded queue filled
  while continuing to report `Streaming`. An incomplete time series looked
  complete.
- The recording status named the configured output path, but the pipeline
  writes the path that survived timestamping and collision avoidance.
- `Status & warnings` reported "All clear" while a plugin sat in the Plugin
  Manager with an ABI-mismatch error.
- `Load Config…` during Preview replaced the shown configuration and cleared
  the dirty flags without touching the running camera, so the panel claimed to
  be in sync with hardware that ran different settings.

Individually these are small. Together they define the failure mode this
repository already rejects for the capture path (ADR 030: "unavoidable loss
must be measured, never silent") and for bias values (ADR 029: "never
substitute a computed guess for a missing monitor value"), applied to the
presentation layer instead of the data path.

## Decision

**A displayed quantity carries its own provenance.** Where a value's
trustworthiness varies, the type that produces the value also produces the
label, so no call site can render the number while forgetting the caveat.

- **`PixelScale { nm_per_pixel, calibrated }`** replaces the bare `f64` that
  used to flow into the viewer. `GlobalSettingsConfig` gains
  `pixel_scale_calibrated: bool`, `#[serde(default)]` so every existing config
  file deserialises as *uncalibrated*. The default is `false` — the sensor
  pitch is the sample-plane scale only for direct detection, and the program
  cannot know whether optics are in the path. Settings ▸ Pixel scale carries a
  `Calibrated for this setup` checkbox, and the menu entry itself reads
  `Pixel scale (nm/px) — uncalibrated…` until it is ticked.
- **`RulerMeasurement::label()` owns its own formatting**, appending
  `(uncal.)`. The scale bar and the line-profile length note do the same
  through `PixelScale::suffix()`. There is no code path that prints
  micrometres without consulting the flag.
- **Axis labels name the physical quantity, not a borrowed one.**
  `histogram_quantity(PreviewMode)` returns the axis label and a one-sentence
  statement of what a bin counts, per preview mode. The line profile plots
  Euclidean distance from the line start (`LineProfileTool::profile_distance_px`),
  not the sample index.
- **A bridge that drops frames counts them.** `ExternalTool::throughput()`
  returns `ExternalToolThroughput { frames_offered, frames_dropped }`. Dropping
  under back-pressure stays the correct behaviour for a preview bridge — the
  alternative is stalling the capture path — but the ImageJ chip changes tone
  and the dialog states the delivered/dropped split.
- **The resolved path is the displayed path.** `CameraApp::active_recording_path`
  holds what the pipeline actually opened; the status bar, the idle message and
  the sensor-telemetry companion name all derive from it. The configured field
  is a request.
- **The health summary is the union of all health.** Plugin load failures join
  analysis warnings, host-view resolution warnings and the last error in
  `Status & warnings`; "All clear" now means all of them are empty.
- **Loading a config while a pipeline runs marks the panel dirty** instead of
  clean, because the two have just diverged.

## Consequences

- Existing `augur.toml` files keep working and read as uncalibrated. Users with
  a genuinely direct-detection setup tick the box once and the marks disappear.
- Every micrometre readout in a screenshot now states whether it is calibrated.
  That is deliberately visible: an unlabelled µm value in a thesis figure is
  the failure this ADR exists to prevent.
- `ExternalTool` implementors must supply `throughput()`. There is no default
  implementation, so a new bridge cannot silently opt out of drop accounting.
- Plot axes changed wording. Anyone matching on the old labels in scripted
  screenshots must update; the underlying data is unchanged.

## Related

- ADR 029: Sensor Monitoring Readback For Absolute Setting Values
- ADR 030: Reader-Owned Transfer Buffers And Recording Completeness Accounting
- `docs/features/pixel-scale-calibration.md`
- `docs/features/viewer-tools-and-imagej.md`
