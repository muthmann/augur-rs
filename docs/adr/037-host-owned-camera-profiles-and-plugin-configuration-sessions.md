# ADR 037: Host-Owned Camera Profiles and Plugin Configuration Sessions

## Status

Accepted (2026-08-12).

## Context

Plugins can coordinate unattended recordings that need reproducible camera
settings, but the settings and the camera remain host-owned. Copying a profile
into plugin-local settings would create a second source of truth. Filling the
settings panel without applying it would leave an unattended operation waiting
for an operator click.

The host must remain a standalone, general-purpose recorder. It must not know
which plugin requested a configuration, which fields matter to that plugin, or
which scientific conditions make a recording valid. A command for one named
workflow or a special two-bias command would put domain policy in the host and
would have to grow another special case for the next plugin.

## Decision

The host stores named, versioned camera/global profiles below its application
configuration directory. Schema version 1 contains the complete `CameraConfig`,
including `global.record_sensor_telemetry`. Saves use a same-directory temporary
file, `sync_all`, rename, and directory sync. A save over an existing name raises
the profile revision. Invalid names, invalid camera values, corrupt TOML, and
unknown schema versions fail closed.

Plugins use two generic host commands, declared as capabilities in their
manifest:

- `ApplyCameraConfiguration` selects the current applied configuration, one
  named profile, or one immutable complete snapshot.
- `RestoreCameraConfiguration` restores the configuration that was active
  before the owning plugin began the session.

The apply command is an operation, not a UI draft. The host rejects existing
unapplied operator edits, applies the requested values immediately through the
normal live reconfigure path, clears the dirty state, and waits for a sensor
read taken after the change. All five bias codes must match. If a setting cannot
change on the live control path, the host restarts Preview with the requested
configuration itself. It does not wait for an operator action. The UI shows the
applied values and reports sensor confirmation only after the fresh readback
succeeds.

Only one plugin owns a configuration session. The owner can replace the active
configuration with another complete snapshot while keeping the original
pre-session configuration for the final restore. This is the generic mechanism
for any per-step change; there are no field-specific host commands. Recordings
started by that plugin receive the active resolved snapshot plus source, profile
name, schema, revision, and SHA-256 provenance. Later edits to a named profile
therefore cannot reinterpret an old recording.

If the apply readback fails, the host reapplies the pre-run configuration and
does not return the original rejection until that rollback has its own fresh
readback. Stop, abort, and normal completion use the same explicit restore
command. A restore without confirmation is an error, not success.

The host enforces only generic safety and ownership rules: no camera change
during recording/finalization, no overwritten pending operator edits, one
session owner, schema and camera-range validation, and fresh readback. A plugin
must enforce its own scientific gates, such as required telemetry capture,
disabled event filters, or which fields it permits its protocol to vary.

## Consequences

Profiles remain host-owned and reusable by the UI and measurement plugins.
Plugin changes are visible as applied settings without another user action.
Older camera TOML without `record_sensor_telemetry` remains readable and
defaults that field to `false`.

Removing every plugin still leaves a complete recorder and profile UI. Adding a
new plugin does not require a plugin ID, workflow name, or scientific policy in
`augur-gui`.

## References

- ADR 012: Generic Plugin Boundary With Companion Domain-Type Crates
- ADR 028: Plugin to Host Command Queue
- ADR 029: Sensor Monitoring Readback
- ADR 031: Optional Sensor-Telemetry Companion Recording
