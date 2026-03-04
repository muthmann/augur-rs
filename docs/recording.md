# Recording Format

AugurRS records raw event data in EVT3 format and writes the active camera configuration next to each capture.

## Output Files

Recording `captures/run.raw` produces:

- `captures/run.raw`
- `captures/run.toml`

The `.toml` sidecar stores the configuration used for that capture so recordings remain reproducible.

## RAW Header

Each `.raw` file starts with EVT3 metadata lines for the IMX636 sensor geometry:

```text
% format EVT3;width=1280;height=720
% geometry 1280x720
% evt 3.0
% end
```

## CLI Behavior

The CLI prints current throughput once per second while recording:

- current Mev/s
- current MB/s
- elapsed time
- total GB written

Stop recording with `Ctrl+C` or `--duration-s`.

## Pipeline Behavior

The recording path is designed to stay bounded under load:

- disk writes use bounded backpressure instead of unbounded buffering
- preview delivery is lossy so it does not block USB streaming
- worker errors are surfaced back to the CLI or GUI session

That split keeps capture stability ahead of preview smoothness when the host is under pressure.
