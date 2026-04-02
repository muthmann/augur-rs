# Recording Format

AugurRS records raw event data in EVT3 format and writes a companion sidecar that keeps the active camera configuration together with recording metadata.

## Output Files

Recording `captures/run.raw` produces:

- `captures/run.raw`
- `captures/run.toml`

The `.toml` sidecar stores the configuration used for that capture so recordings remain reproducible.
That includes the host-owned `[global]` settings block that records pixel scale, configured sensor
geometry, acquisition time, retained history budget, and advanced GUI/runtime tuning, plus a
`[metadata]` table with device identity, software provenance, and post-recording timing fields.

## RAW Header

Each `.raw` file starts with EVT3 metadata lines for the configured sensor geometry plus the
recorded device/provenance fields:

```text
% format EVT3;width=<sensor_width>;height=<sensor_height>
% geometry <sensor_width>x<sensor_height>
% evt 3.0
% serial_number <camera-serial>
% system_id <vendor-and-model>
% firmware_version <firmware>
% sensor_compatible <sensor-tags>
% augur_version <augur-version>
% recording_date <utc-rfc3339>
% recording_hostname <host-name>
% pixel_pitch_nm <nm-per-pixel>
% end
```

If the user leaves the geometry untouched, those values default to `1280x720`. Older `.raw` files
that only contain geometry still open normally, and older Augur builds skip the newer metadata lines
without breaking replay.

## Sidecar Metadata

The sidecar keeps the effective camera config at the top level and adds a `[metadata]` table with:

- the same device identity and provenance values written into the EVT3 header
- `recording_duration_us`, computed from the first and last decoded event timestamps seen during recording
- `total_events`, derived from the pipeline event counter
- optional `annotations` such as `experiment_id`, `operator`, and `notes`

## CLI Behavior

The CLI prints current throughput once per second while recording:

- current Mev/s
- current MB/s
- elapsed time
- total GB written

Stop recording with `Ctrl+C` or `--duration-s`.

## Pipeline Behavior

The recording path is designed to stay bounded under load:

- output files and EVT3 headers are prepared before streaming starts
- disk writes use bounded backpressure instead of unbounded buffering
- already-read packets are still drained into the disk queue during shutdown
- preview delivery is lossy so it does not block USB streaming
- worker errors are surfaced back to the CLI or GUI session

That split keeps capture stability ahead of preview smoothness when the host is under pressure. `MB/s` remains the authoritative throughput indicator; `Mev/s` is now derived from the decoded preview side so the USB reader does not spend capture time scanning every packet.
