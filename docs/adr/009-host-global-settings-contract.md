# ADR 009: Host-Owned Global Settings Contract

## Status

Accepted

## Context

Several host-owned runtime settings had become fragmented:

- pixel scale was only present in downstream plugin-specific payloads
- acquisition time lived in the `Camera` menu and in a separate runtime field
- the EventStore budget lived in the left settings panel
- user-facing sensor geometry was duplicated as hardcoded `1280x720` values across the GUI

At the same time, runtime plugins needed a stable way to read those settings without extending the
FFI surface yet again.

## Decision

Adopt one persisted host-owned global-settings contract:

1. `CameraConfig` gains a serde-defaulted `[global]` block for host/runtime settings.
2. `augur-gui` owns the editable runtime copy of those settings and synchronizes it to
   `CameraConfig.global` at save/load and pipeline-boundary points.
3. Runtime plugins read the current host settings through the existing per-frame JSON context bus
   under `augur.global_settings` as `augur_plugin_api::GlobalSettings`.
4. Replay-speed changes use a lightweight `speed_epoch` reset rather than changing the replay
   control surface or introducing a more expensive scheduling model.

## Consequences

### Positive

- users get one obvious place to find pixel scale, geometry, acquisition time, retention
  budget, and cadence controls
- runtime plugins can consume host-owned settings without an ABI change
- old TOML sidecars and configs remain readable because `[global]` is defaulted
- replay speed changes become responsive without abandoning the existing byte-rate throttle model

### Negative

- the GUI still keeps runtime fields alongside persisted config, so explicit synchronization is part
  of the host contract
- sensor geometry and disk-writer buffer are start-time controls, not fully live-reconfigurable
- plugins that want to rely on `GlobalSettings` still need to tolerate `None` when run against an
  older host

## Alternatives Considered

### Extend the runtime FFI/vtable again

Rejected because the context bus already supports typed shared payloads, and this change does not
need another ABI migration.

### Keep each setting in its old GUI location

Rejected because the scattered UI made it harder for users to discover the effective host
parameters and harder for the host to persist them coherently.
