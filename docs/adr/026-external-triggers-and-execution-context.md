# ADR 026 — External-trigger delivery and plugin execution context (ABI v5)

- **Status:** Accepted
- **Date:** 2026-07-13

## Context

Stage-A camera-calibration protocols (A1–A3) synchronize the camera to an
external stimulus through the EVK4 `TRIG_IN` pin: the sensor inserts EVT3
`EXT_TRIGGER` words into the event stream, timestamped on the same clock as
CD events. `evt3-core` already decoded these words, but every pipeline path
(live preview, RAW replay, offline analysis) discarded them before plugins
could observe them.

Separately, companion plugins that own laboratory hardware (Pockels-cell
drive via a Teensy) need a host-provided guarantee that they only open
devices in the *active live-capture worker* — never during replay, offline
analysis, or in secondary GUI plugin instances. No such signal existed in
the plugin ABI.

Both changes touch `FfiPreviewFrame` and `FfiPluginContext`, so they are
delivered as **one deliberate ABI bump (v4 → v5)**.

## Decision

1. **`ExternalTriggerEvent` is a public, ABI-stable event type** in
   `augur-event-types`: a fixed 16-byte `#[repr(C)]` record
   `{ timestamp_us: u64, id: u8, level: u8, _reserved: [u8; 6] }` with an
   ergonomic `is_rising()` accessor. The same type is used in-process and
   across the dynamic plugin boundary (`FfiExternalTriggerEvent` alias).

2. **Rollover-epoch mapping for sparse secondary streams.** Trigger events
   arrive in a separate vector from CD events and are too sparse to
   self-unwrap across the 24-bit EVT3 rollover (~16.8 s). A
   `SecondaryTimestampMapper` assigns each trigger the rollover candidate
   nearest to the CD unwrapper's current reference (falling back to the last
   mapped trigger), which is exact while packets are shorter than half a
   period. Feeding triggers through the CD unwrapper itself is explicitly
   rejected: it would either count spurious wraps or clamp mid-packet
   trigger times forward to the packet's last CD timestamp.

3. **Frame-scoped delivery.** `PreviewFrame` (and its FFI mirror) carries
   the trigger edges whose timestamps fall inside the frame's accumulation
   window. Windows can open and close on trigger time alone, so
   trigger-only streams still produce frames, and pending trigger edges are
   flushed in a final partial frame at end of stream. CD-only partial tail
   windows keep the historical behavior (no tail frame). Live preview
   delivery remains best-effort (a dropped preview frame drops its triggers);
   exact results are recomputed from the recorded RAW file, where
   `RawReplayEventSource::fetch_range` now returns triggers in `EventChunk`
   and a trigger-only window is a valid (non-`OutOfTimeline`) result.
   Trigger edges also advance replay time and count toward the recording
   duration, so trigger-only recordings play back. Decoded (CSV/HDF5/packed)
   imports expose an empty trigger slice in v1 — a stated limitation.

4. **`ExecutionContext` in the plugin ABI.**
   `FfiPluginContext.execution` carries
   `{ mode: LiveCapture | Replay | OfflineAnalysis, effects_allowed, session_id }`.
   The safe wrapper exposes `HostContext::execution()` and
   `ExecutionContext::hardware_effects_allowed()`
   (`mode == LiveCapture && effects_allowed`). Hosts construct it
   fail-closed: only the live-capture worker job sets
   `LiveCapture + effects_allowed`; the synchronous GUI executor, every
   replay pass, and offline analysis are always effects-free. This is a
   generic safety boundary, not an instrument API — AugurRs still gains no
   Teensy or serial abstraction (device ownership stays in companion
   plugins, per the Stage-A control-software spec).

5. **Sensor enable is configuration.** `CameraConfig.external_triggers`
   (`enabled`, `channel`; channel 0 only) maps to the OpenEB-derived IMX636
   register sequence (`Gen41TzTriggerEvent::enable`). Toggling while
   streaming is rejected.

## Consequences

- `PLUGIN_ABI_VERSION` is 5; all companion plugins must be rebuilt against
  the new `augur-plugin-api`. The vtable itself is unchanged — the bump
  guards the `FfiPreviewFrame` / `FfiPluginContext` layout change.
- `EventChunk` gained a `triggers` field; every `EventSource` implementation
  fills it (empty for sources without a trigger concept).
- `accumulate_compact_frame` takes the window's trigger slice.
- Plugins can rely on: triggers observed in replay/offline analysis are
  exact; triggers observed live are best-effort and must not be used for
  final results.

## Verification

Unit tests cover: EXT_TRIGGER decode (rising/falling edge + channel id),
rollover-straddle epoch assignment (with and without CD events in the
trigger's packet), frame-window assignment, trigger-only frame emission and
EOF flush, RAW-replay range fetch incl. trigger-only windows, trigger-only
recording duration/replay-time advance, ExecutionContext FFI round-trip,
IMX636 register sequences, and ABI layout/version guards.
