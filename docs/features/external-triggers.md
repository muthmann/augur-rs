# External triggers & plugin execution context

> Feature brief — plugin ABI v5 (ADR 026)

## What it does

- **Sensor:** the EVK4 `TRIG_IN` input (channel 0) can be enabled in
  Settings → External Triggers (or `CameraConfig.external_triggers` in
  TOML). Each electrical edge inserts an EVT3 `EXT_TRIGGER` event into the
  stream, timestamped on the same clock as pixel events. The toggle cannot
  change while streaming.
- **Delivery:** trigger edges are carried through live decode, RAW
  recording/replay, and offline analysis, and delivered to plugins as
  `frame.external_triggers()` (`&[FfiExternalTriggerEvent]`,
  `{timestamp_us, id, is_rising()}`) beside `frame.events()`.
- **Execution context:** plugins receive
  `HostContext::execution()` → `{ mode, effects_allowed, session_id }`.
  `hardware_effects_allowed()` is `true` only in the active live-capture
  worker. Hardware-owning plugins (Stage-A Teensy protocols) must fail
  closed on everything else — a replayed recording can never re-arm
  laboratory hardware.

## Guarantees and limitations

| Path | Trigger delivery |
|------|------------------|
| Live preview / live worker | Best-effort (a dropped preview frame drops its triggers) |
| RAW recording → replay | Exact — recompute final results from the `.raw` file |
| Offline analysis (RAW input) | Exact, windowed like CD events |
| Decoded imports (CSV/HDF5/packed) | Not carried in v1 (empty slice) |

- Trigger-only recordings report a duration, advance replay time, produce
  preview frames, and return trigger-only (CD-empty) windows from
  `fetch_range` — required for A2-style latency protocols.
- Trigger timestamps are unwrapped across the 24-bit EVT3 rollover against
  the CD stream's epoch (see ADR 026 for why they cannot self-unwrap).
- Only trigger channel 0 is supported; other channels are rejected at
  config validation.

## Consumers

The Stage-A calibration plugins (`stage-a-*` in augur-plugins): A1/A3 phase
folding on the Teensy phase-0 TTL, A2 step-latency timing on the
photodiode-comparator edge. See the knowledge-base spec
(`methodology/stage-a-control-software.md`) for the protocol side.

## Key code

- `augur-event-types`: `ExternalTriggerEvent`, `EventChunk.triggers`
- `augur-core/src/evt3_timestamps.rs`: `SecondaryTimestampMapper`
- `augur-core/src/pipeline.rs`: decoder + frame windowing + EOF flush
- `augur-core/src/replay.rs`: replay fetch, duration scan, time advance
- `augur-prophesee/src/sensors/imx636.rs`: enable/disable register sequences
- `augur-plugin-api`: `FfiPreviewFrame.external_triggers`,
  `FfiPluginContext.execution`, `ExecutionContext`, ABI v5
- `augur-runtime`: `LiveAnalysisJob.execution`, offline pass context
