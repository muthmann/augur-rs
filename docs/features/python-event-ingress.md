# Python Event Ingress

## Summary

Augur can accept NumPy event arrays published from Python through the
`evt3.augur` connector. This first integration stage is intentionally
copy-based: Python validates and packs each event chunk as `packed_xypt_v1`,
then Augur receives those chunks over a loopback TCP protocol and feeds them
through the existing decoded-event preview pipeline.

The user-facing workflow is:

```python
import evt3

events = evt3.decode_file("recording.raw")
evt3.augur.publish_events(events, name="recording-analysis-window")
```

Augur then renders the published events with the normal 2D preview, 3D
raw-event view, viewer tools, and runtime plugin path.

## GUI Workflow

1. In Augur, choose `Tools -> Listen for Python Events...`.
2. In Python, call `evt3.augur.publish_events(...)`.
3. Augur starts an external preview stream using the published geometry and
   current acquisition-window setting.
4. When the Python stream finishes, Augur leaves the last frame visible for
   inspection.
5. Use `Stop` to clear the current stream before returning to camera preview,
   recording, or replay.

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

Augur advertises a maximum batch size of `1_048_576` events. The listener uses
a bounded internal packet queue, so Python backpressures on batch acknowledgments
instead of causing unbounded GUI memory growth.

## Architecture

The first cut reuses the decoded replay path rather than introducing a second
event pipeline:

- `augur-gui/src/python_ingress.rs` owns the loopback listener, protocol parser,
  bounded packet queue, and `PacketStreamCamera` adapter.
- The app starts `spawn_pipeline(..., PackedEventPreviewDecoder::default(), ...)`
  after accepting a `start_events` message.
- The existing pipeline appends decoded events to `LiveEventSource`, emits
  `PreviewFrame`s, and keeps downstream viewer/plugin behavior unchanged.

This means the first implementation performs one copy into Augur's packed
transport path. Cross-process zero-copy and direct external `EventSource`
replacement remain future work.

## Validation

Augur validates the protocol envelope:

- protocol version
- record format and record byte size
- geometry width/height
- time unit
- batch byte count
- maximum chunk size

dtype, array shape, timestamp monotonicity, and coordinate range validation are
handled in the evt3 Python connector before bytes are sent.

## Files

| File | Role |
|---|---|
| `augur-gui/src/python_ingress.rs` | Python ingress listener, protocol parser, packed packet camera |
| `augur-gui/src/app.rs` | Tools menu wiring, stream acceptance, pipeline startup, status chip |
| `docs/adr/021-python-event-ingress.md` | Architecture decision for the first-stage copy-based ingress |

## Verification

- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `git diff --check`
- `cargo test -p augur-gui python_ingress --locked`
- `cargo test -p augur-gui python_ingress_pipeline_config --locked`
- `cargo check -p augur-gui --bin AugurRS`
- `cargo test -p augur-gui --locked`
