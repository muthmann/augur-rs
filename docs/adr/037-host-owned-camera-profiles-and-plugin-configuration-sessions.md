# ADR 037: Host-Owned Camera Profiles and Plugin Configuration Sessions

## Status

Accepted (2026-08-12).

## Context

Measurement plugins need reproducible camera settings, but the settings and the
camera remain host-owned. Copying a profile into plugin-local settings would
create a second source of truth. Filling the settings panel without applying it
would also leave an unattended protocol waiting for an operator click.

## Decision

The host stores named, versioned camera/global profiles below its application
configuration directory. Schema version 1 contains the complete `CameraConfig`,
including `global.record_sensor_telemetry`. Saves use a same-directory temporary
file, `sync_all`, rename, and directory sync. A save over an existing name raises
the profile revision. Invalid names, invalid camera values, corrupt TOML, and
unknown schema versions fail closed.

Plugins use two allowlisted host commands:

- `ApplyCameraConfiguration` selects exactly one named profile or immutable
  inline snapshot.
- `RestoreCameraConfiguration` restores the configuration that was active
  before the owning plugin began the session.

The apply command is an operation, not a UI draft. The host rejects existing
unapplied operator edits, applies the requested values immediately through the
normal live reconfigure path, clears the dirty state, and waits for a sensor
read taken after the change. All five bias codes must match. The selected
configuration must enable sensor telemetry. If a setting cannot change on the
live control path, the host restarts Preview with the requested configuration
itself. It does not wait for an operator action. The UI shows the applied values
and reports sensor confirmation only after the fresh readback succeeds.

Only one plugin owns a configuration session. Recordings started by that plugin
receive the resolved snapshot plus source, profile name, schema, revision, and
SHA-256 provenance. Later edits to the named profile therefore cannot reinterpret
an old recording.

If the apply readback fails, the host reapplies the pre-run configuration and
does not return the original rejection until that rollback has its own fresh
readback. Stop, abort, and normal completion use the same explicit restore
command. A restore without confirmation is an error, not success.

The existing narrow `ApplyBiases` command remains the per-point path for
`diff_on` and `diff_off`. It does not widen to unrelated camera controls.

## Consequences

Profiles remain host-owned and reusable by the UI and measurement plugins.
Plugin changes are visible as applied settings without another user action.
Older camera TOML without `record_sensor_telemetry` remains readable and
defaults that field to `false`; older A1 protocols without camera fields are
unchanged.

## References

- ADR 028: Allowlisted Plugin to Host Recording Commands
- ADR 029: Sensor Monitoring Readback
- ADR 031: Optional Sensor-Telemetry Companion Recording
- ADR 036: Plugin Bias Apply Command
