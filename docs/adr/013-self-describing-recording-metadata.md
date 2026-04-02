# ADR 013: Self-Describing Recording Metadata

## Status
Accepted

## Context

Augur recordings previously wrote only minimal EVT3 header geometry and kept the rest of the
capture context in a separate config sidecar. That made `.raw` files opaque on their own and
forced replay to fall back to generic device labels even though the driver already knows the
camera serial, firmware, vendor, model, and pixel scale.

## Decision

Adopt a shared `RecordingMetadata` contract in `augur-core`:

1. Write device identity and software provenance into the EVT3 header as `% key value` lines.
2. Persist a `[metadata]` table in the recording sidecar alongside the existing flattened
   `CameraConfig` sections.
3. Preserve unknown header keys in an extensible `extra` map instead of rejecting them.
4. Restore raw replay `DeviceInfo` and replay defaults from parsed recording metadata.

Timing fields such as `recording_duration_us` and `total_events` stay in the sidecar rather than in
the EVT3 header.

## Consequences

- New `.raw` files are self-describing even without their sidecar.
- Raw replay can surface recorded device identity instead of a generic replay placeholder.
- Existing code that deserializes sidecars as `CameraConfig` keeps working because `[metadata]` is a
  separate table that Serde ignores by default.
- The recording format becomes easier to extend over time without breaking old parsers.
