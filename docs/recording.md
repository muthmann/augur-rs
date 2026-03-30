# Recording Format

AugurRS records raw event data in EVT3 format and writes the active camera configuration next to each capture.

## Output Files

Recording `captures/run.raw` produces:

- `captures/run.raw`
- `captures/run.toml`

The `.toml` sidecar stores the configuration used for that capture so recordings remain reproducible.
That includes the host-owned `[global]` settings block that records pixel scale, configured sensor
geometry, acquisition time, retained history budget, and advanced GUI/runtime tuning.

## RAW Header

Each `.raw` file starts with EVT3 metadata lines for the configured sensor geometry:

```text
% format EVT3;width=<sensor_width>;height=<sensor_height>
% geometry <sensor_width>x<sensor_height>
% evt 3.0
% end
```

If the user leaves the geometry untouched, those values default to `1280x720`.

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
