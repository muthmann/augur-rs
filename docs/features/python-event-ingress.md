# Python Event Ingress

## Summary

Augur can accept NumPy event arrays published from Python through the
`evt3.augur` connector. This first integration stage is intentionally
copy-based: Python validates and packs each event chunk as `packed_xypt_v1`,
then Augur receives those chunks over a loopback TCP protocol, decodes them
into an in-memory event timeline, and opens that timeline through the existing
decoded replay pipeline.

The user-facing workflow is:

```python
import evt3

events = evt3.decode_file("recording.raw")
evt3.augur.publish_events(events, name="recording-analysis-window")
```

Augur then renders the published events with the normal 2D preview, 3D
raw-event view, viewer tools, replay transport, and runtime plugin path.

## GUI Workflow

1. In Augur, choose `Tools -> Listen for Python Events...`.
2. In Python, call `evt3.augur.publish_events(...)`.
3. Augur receives and validates the finite dataset.
4. When publication finishes, Augur opens the dataset as an in-memory decoded
   replay, decodes the first frame, and pauses.
5. Use the normal replay controls to play, pause, seek, step, restart, inspect
   the 2D preview, and inspect the 3D raw-event scene.
6. Use `Close Replay` or camera `Stop` to clear the Python dataset before
   returning to camera preview, recording, or a file replay.

The listener binds only to `127.0.0.1` and defaults to port `57295`.

## Protocol

The protocol is versioned and loopback-only:

- `hello` / `hello_ok`
- `start_events` / `start_ok`
- repeated `event_batch` messages followed by binary payloads
- `finish_events` / `finish_ok`

Only `record_format = "packed_xypt_v1"` is accepted in this stage. Each event
record is 14 little-endian bytes:

| Offset | Type | Meaning |
|---:|---|---|
| 0 | `u16` | x |
| 2 | `u16` | y |
| 4 | `u8` | polarity |
| 5 | `u8` | padding |
| 6 | `u64` | timestamp in microseconds |

Augur advertises a maximum batch size of `1_048_576` events. Each batch is
acknowledged only after Augur has received, size-checked, and decoded it into
the in-memory dataset. The final `finish_ok` is sent only after the GUI accepts
the completed dataset as a replay.

## Architecture

The first cut reuses the decoded replay path rather than introducing a second
event pipeline:

- `augur-gui/src/python_ingress.rs` owns the loopback listener, protocol parser,
  batch decoding, and completed-dataset handoff.
- The app opens the completed dataset with `DecodedEventFileCamera::open_at`
  and starts `spawn_pipeline(..., PackedEventPreviewDecoder::default(), ...)`
  in replay mode.
- The existing replay pipeline appends decoded events to `LiveEventSource`,
  emits `PreviewFrame`s, and keeps downstream viewer/plugin behavior unchanged.

This means the implementation performs one copy into Augur's in-memory decoded
timeline. Cross-process zero-copy and direct external `EventSource` replacement
remain future work.

## Validation

Augur validates the protocol envelope:

- protocol version
- record format and record byte size
- geometry width/height
- time unit
- batch byte count
- maximum chunk size
- coordinate bounds against the published geometry
- final event count against `start_events.event_count`

dtype, array shape, and timestamp monotonicity validation are handled in the
evt3 Python connector before bytes are sent.

## Files

| File | Role |
|---|---|
| `augur-gui/src/python_ingress.rs` | Python ingress listener, protocol parser, packed batch decoder |
| `augur-gui/src/app.rs` | Tools menu wiring, dataset acceptance, in-memory replay startup, status chip |
| `docs/adr/021-python-event-ingress.md` | Architecture decision for the first-stage copy-based ingress |

## Verification

- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p augur-gui python_ingress --locked`
- `cargo check -p augur-gui --bin AugurRS`
- `cargo test -p augur-gui --locked`
