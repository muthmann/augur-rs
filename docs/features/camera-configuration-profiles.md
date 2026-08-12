# Camera Configuration Profiles

Augur stores named camera/global profiles in the application configuration
directory under `augur/camera-profiles`. Each TOML file has `schema_version = 1`,
a profile name, a monotonic revision, and one complete `CameraConfig`.

Use **File → Named Profiles** to enter a name, save a new profile, update an
existing profile, load it into the settings panel, or delete it. Loading from
the menu is an operator edit and uses the normal **Apply** action. In contrast,
a plugin that declares the generic camera-configuration capability applies a
referenced profile immediately; no extra operator action is required.

The stored global values include **Record sensor monitoring**. Profiles written
before this field existed still load with the safe default `false`. A plugin
that needs telemetry for scientific validity checks this value in the confirmed
snapshot and refuses its own operation; this is not host policy.

Plugin application is fail closed:

- pending operator edits are never overwritten;
- only one plugin can own a configuration session;
- live-safe settings are applied through the normal camera-control path;
- settings that need a pipeline restart restart Preview automatically;
- a fresh sensor readback must confirm all five bias codes;
- failed confirmation triggers a confirmed rollback; and
- success, Stop, and abort restore the pre-run configuration.

The session owner may apply another complete snapshot while the session is
active. The host still retains the original pre-session configuration. This
keeps field-specific rules in the plugin and avoids field- or plugin-specific
host commands.

Each plugin-started recording stores the resolved immutable snapshot and its
profile name, schema, revision, and SHA-256 provenance. Changing the saved
profile later does not change the meaning of an existing measurement.

See [ADR 037](../adr/037-host-owned-camera-profiles-and-plugin-configuration-sessions.md).
