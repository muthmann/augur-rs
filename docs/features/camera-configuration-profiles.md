# Camera Configuration Profiles

Augur stores named camera/global profiles in the application configuration
directory under `augur/camera-profiles`. Each TOML file has `schema_version = 1`,
a profile name, a monotonic revision, and one complete `CameraConfig`.

Use **File → Named Profiles** to enter a name, save a new profile, update an
existing profile, load it into the settings panel, or delete it. Loading from
the menu is an operator edit and uses the normal **Apply** action. In contrast,
an allowlisted measurement plugin applies a referenced profile immediately; no
extra operator action is required.

The stored global values include **Record sensor monitoring**. A measurement
profile that disables it cannot be applied by a plugin that requires confirmed
camera settings. Profiles written before this field existed still load with the
safe default `false`.

Plugin application is fail closed:

- pending operator edits are never overwritten;
- only one plugin can own a configuration session;
- live-safe settings are applied through the normal camera-control path;
- settings that need a pipeline restart restart Preview automatically;
- a fresh sensor readback must confirm all five bias codes;
- failed confirmation triggers a confirmed rollback; and
- success, Stop, and abort restore the pre-run configuration.

Each plugin-started recording stores the resolved immutable snapshot and its
profile name, schema, revision, and SHA-256 provenance. Changing the saved
profile later does not change the meaning of an existing measurement.

See [ADR 037](../adr/037-host-owned-camera-profiles-and-plugin-configuration-sessions.md).
