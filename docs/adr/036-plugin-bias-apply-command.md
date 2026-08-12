# ADR 036: A Plugin May Change Two Biases, and Only Against a Readback

## Status

Accepted (2026-08-07), implemented as the `apply_biases` verb on the existing
`HostCommand` queue (ABI v6 — the control plane is JSON, so the vtable is
unchanged).

## Context

A contrast-threshold survey (Stage-A A4) sets `diff_on`/`diff_off` to each of a
list of values, records a RAW file at each, and later reads the event rate
against the threshold. It is a measurement whose independent variable *is* a
camera bias.

Until now a plugin could start and stop a recording (ADR 028) but not touch the
sensor, so the survey meant an operator moving two sliders and pressing Apply
between every point, for dozens of points, with the actual programmed codes
recorded by hand. That is slow, and — more importantly — the provenance is
unverifiable: the settings panel shows the *requested* offset, while the
quantity the measurement depends on is the absolute code sitting in the bias
register.

The obvious shapes are both wrong. A general "set any camera setting" verb hands
dynamic plugins the whole sensor configuration, when what is needed is two
fields. And a fire-and-forget "set these biases" verb answers the question the
measurement is not asking: whether the *request* was accepted, rather than what
the sensor is running.

## Decision

**The command is two fields wide.** `ApplyBiases { diff_on, diff_off }`, offsets
around the factory trim exactly as the settings panel expresses them, each
optional. There is no wire field for `fo`, `hpf`, `refr`, the ROI or the pixel
mask, so a threshold survey cannot disturb them even by mistake — the freeze is
structural rather than a rule someone has to follow. Widening this later is a
deliberate act with its own review; it is not something a plugin can reach by
sending a different payload.

**The reply is a readback, not an acknowledgement.** The host writes the config
through the operator's own Apply path, then parks the request and waits for a
monitoring read. It answers `BiasesApplied` with the absolute codes the sensor
reported, the factory trim they are offset from, and the age of that reading.

A snapshot `age_s` old was taken at `now - age_s`, so it may confirm a change
made `since_apply` ago only when `age_s <= since_apply`. Without that test the
codes from *before* the write — which are always sitting there, a few hundred
milliseconds stale — would be accepted as proof of the write. Codes that
disagree with `factory_default + offset` are `bias_readback_mismatch`; a reading
that never arrives within five seconds is `bias_readback_unavailable`. Neither
is a receipt.

**The host owns the interlocks**, because a plugin cannot enforce them:

- `recording_active` — biases never move during a recording or its
  finalization. Two thresholds inside one file cannot be separated afterwards.
- `event_filters_enabled` — STC or Trail on refuses. They discard events before
  streaming, which is precisely the quantity being counted.
- `no_camera`, `bias_apply_busy`, `bias_out_of_range` (`-85..=140`, mirroring
  the IMX636 driver).

**Filter state becomes visible to plugins.** `GlobalSettings` gains
`event_filters` (`stc_enabled`, `trail_enabled`, `erc_enabled`) so a survey can
refuse *before* it starts rather than being rejected on its first point, and so
the state lands in provenance. This host has no event-rate controller, so
`erc_enabled` is always `false` — the field exists so a measurement required to
record "ERC was off" records a fact rather than an omission, and so sidecars
stay readable if one is ever added.

## Consequences

A workflow plugin can run a bias survey unattended and prove, per point, which
codes were live on the die. The grant stays legible in `plugin.toml`
(`host_commands = [..., "apply_biases"]`) and the runtime rejects it for any
plugin that has not declared it.

The plugin must handle a rejected point rather than assuming the change landed —
which is the correct posture anyway, since a mismatched readback means the point
is not measuring what the protocol says it measures.

A host and a plugin built against different API revisions still interoperate for
recording: `event_filters` is `#[serde(default)]`, and an old host that does not
know the verb simply never answers it, which the plugin resolves with its own
reply timeout.

## References

- ADR 028: Allowlisted Plugin to Host Recording Commands
- ADR 029: Sensor Monitoring Readback
- `augur-plugins` ADR 034: Stage-A A4 bias threshold survey
