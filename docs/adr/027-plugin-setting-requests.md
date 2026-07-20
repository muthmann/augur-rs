# ADR 027: Host-Routed Plugin Setting Requests

## Status

Proposed (2026-07-17). Not implemented — this ADR records the intended design
so it is not re-derived ad hoc when the first cross-plugin use case ships.

## Context

The Stage-A bench work surfaced a recurring desire: let one plugin change
another plugin's settings — e.g. a future experiment plugin driving the
`stage-a-modulation` plugin's wave/level/frequency, or a calibration plugin
nudging the `stage-a-photodiode` reference. Today there is no supported
channel for this:

- The persistent context bus (ADR 006/009) is read/written only during
  `process_frame`, which the host calls **only while camera frames flow**.
  The Stage-A plugins are frame-independent (device threads driven by
  settings), so a frame-gated bus cannot reach them on the bench.
- `HostActionRequest` (ADR 018) flows host → plugin, is scoped to
  dataset/row/cluster selections, and is also consumed in `process_frame`.
  It is the wrong shape and the wrong delivery point for "set another
  plugin's knob".
- The architecture's established answer to "plugin A needs plugin B's
  behaviour" is **ownership**, not remote control (ADR 012, and ADR 006 in
  augur-plugins): a plugin that needs the command port links `stage-a-io`
  and owns the port itself. That remains the preferred pattern where it
  applies. This ADR covers the residual case where two *independently
  useful* plugins must coordinate without merging.

## Decision

Add a **host-routed setting request** channel, delivered through the
frame-independent `set_setting` path rather than the frame bus.

- Plugins may emit `PluginSettingRequest { target_plugin, key, value,
  request_id }` via a new `HostOutput`-adjacent queue that the host drains
  every GUI tick (not only during `process_frame`). Emission is possible
  from `set_setting`/`status_entries` context, so a button-driven request
  works with no camera.
- The **host is the single arbiter**. It validates the target exists and is
  enabled, then applies the request by calling the target plugin's
  `set_setting(key, value)` on the UI thread — the exact path a human
  toggle uses, so all existing clamping/validation/immediate-transfer logic
  is reused and no new write path into device state is created.
- **Single-writer arbitration:** a target setting is "claimed" by at most
  one requesting plugin at a time. A second requester for the same
  `(target, key)` is rejected (surfaced as an error to the requester), never
  silently interleaved. Manual UI edits always win and clear the claim —
  the human is the ultimate authority.
- **Visibility is mandatory.** Every applied request raises a toast and a
  log line naming source → target → key = value, and the target plugin's
  settings row shows a "driven by <source>" badge while a claim is held.
  Hidden cross-plugin mutation is the main risk and is designed out.
- Requests are allowlisted per target: a plugin declares
  `accepts_setting_requests: bool` (default false) in its manifest, so a
  plugin cannot be puppeteered unless it opts in.

## Consequences

### Positive

- Frame-independent, so it works on the camera-less bench where the context
  bus does not.
- Reuses `set_setting`, inheriting every plugin's own validation; the host
  learns nothing domain-specific.
- Ownership stays the default; this is the explicit, auditable escape hatch
  for the genuine two-plugin case.

### Negative / Risks

- Introduces a second write path to plugin settings (host UI + peer
  plugin). Arbitration and the "manual wins" rule must be watertight or two
  actors fight over one knob.
- Ordering across a burst of requests is host-tick-quantised; a requester
  wanting a precise sequence must serialise it itself (or own the device).

### Neutral

- The wire type mirrors `HostActionRequest` in the opposite direction, so
  the dedupe-by-`request_id` machinery and JSON-value payloads are already
  familiar.

## References

- ADR 006: Host-Owned Dataset/View Registry for Plugin Outputs
- ADR 009: Host-Owned Global Settings Contract
- ADR 018: Host Action Bus For Plugin-Declared Actions
- ADR 012: Generic Plugin Boundary With Companion Domain-Type Crates
- `docs/features/photodiode-lab-workflow-design.md` (§7)
