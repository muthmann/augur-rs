# Rich Recording Metadata

Augur recordings now carry device identity and provenance in the EVT3 header and persist richer
context in the companion `.toml` sidecar.

## Behavior

### EVT3 header

New recordings add these metadata lines between `% evt 3.0` and `% end`:

- `serial_number`
- `system_id`
- `firmware_version`
- `sensor_compatible`
- `augur_version`
- `recording_date`
- `recording_hostname`
- `pixel_pitch_nm`

That makes a `.raw` file self-describing even without its sidecar.

### Sidecar

The sidecar keeps the existing top-level `CameraConfig` sections and adds a `[metadata]` table.
That table mirrors the header fields and also stores:

- `recording_duration_us`
- `total_events`
- optional `annotations` with `experiment_id`, `operator`, and `notes`

Because the metadata lives in its own table, older code that deserializes the sidecar as
`CameraConfig` still ignores it cleanly.

### Replay

Raw replay now parses those header lines into `ReplayFileInfo::metadata` and uses them to rebuild
the replay `DeviceInfo`. The shared viewer info strip therefore shows the recorded sensor identity,
serial number, and firmware instead of the old generic replay label. If a raw replay is missing its
sidecar, Augur still reuses the recorded `pixel_pitch_nm` value from the header for the default
replay config.

### CLI

`augur record` now accepts optional metadata annotations:

- `--experiment-id`
- `--operator`
- `--notes`

Those values are stored in the sidecar metadata. GUI annotation input is still deferred.

## Compatibility

- Older `.raw` files without the new metadata lines still replay with an empty `RecordingMetadata`.
- Older Augur builds skip the extra `% key value` lines in new `.raw` files without breaking.
- Unknown future header keys are preserved in `RecordingMetadata::extra` so replay parsing stays
  extensible.

## Files

| File | Role |
|---|---|
| `augur-core/src/metadata.rs` | shared metadata model, header serialization, sidecar wrapper |
| `augur-core/src/pipeline.rs` | header writing, sidecar persistence, post-recording timing updates |
| `augur-core/src/replay.rs` | raw header parsing and replay metadata restoration |
| `augur-core/src/decoded_replay.rs` | default empty metadata for decoded replay formats |
| `augur-cli/src/main.rs` | CLI metadata annotations |
| `augur-gui/src/app.rs` | GUI recording metadata hookup and replay pixel-pitch fallback |

## Verification

- `cargo fmt --all`
- `cargo test -p augur-core`
- `cargo clippy --workspace --all-targets -- -D warnings`
