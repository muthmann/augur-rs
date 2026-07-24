# ADR 027: Worker-Owned Semantic Plugin Services

## Status

Accepted (2026-07-20), implemented with dynamic plugin ABI v6.

## Context

Laboratory workflows need to coordinate independently useful device plugins.
For Stage-A, `stage-a-modulation` and `stage-a-photodiode` must remain the only
owners of their Teensy serial ports while an A1 workflow coordinates them.
The frame context bus cannot do this: it runs only when camera frames arrive.
Calling a target plugin's generic `set_setting` on the GUI instance is also
unsafe. The GUI and live worker hold separate plugin instances, so it would
make the inert UI mirror perform hardware effects and would split ownership.

## Decision

Augur provides a frame-independent semantic service control plane:

- A plugin participating in routing declares a stable, unique `id` in
  `plugin.toml`. Display names are never routing identities.
- Only the `LiveWorker` plugin instance receives periodic `process_control`
  calls and `handle_service_request` calls. The worker ticks every 50 ms even
  when no camera frames arrive.
- Requests name a target plugin ID, target-defined semantic service verb,
  request ID, and JSON payload. The host overwrites source identity, routes to
  an enabled target, and returns an explicit accepted/rejected reply.
- Request IDs are idempotency keys scoped to the source plugin. Exact retries
  receive the cached reply; reuse with different content is rejected.
  Because the counter lives in the plugin instance and restarts at 1 when that
  instance is recreated, the reply cache is cleared on plugin reload and on a
  discontinuity. Without that reset a recycled ID could be answered from cache
  **without the target plugin ever running** — a repeated `output_off.v1` would
  report `Accepted` while the device was never touched. The cache is also
  bounded, evicting oldest-first, so a long session cannot grow it without
  limit.
- Targets publish revisioned, read-only control snapshots. The worker sends
  snapshots and authoritative status entries to the GUI; they are not a
  mutable shared-state bus.
- The periodic control result also transports generation-cached host-view
  datasets from the authoritative worker. Analysis and control publications
  share a monotonic sequence; the GUI accepts only the newest publication.
  This keeps device telemetry visible without camera frames and prevents
  cross-channel receive order from rolling a dataset back.
- Runtime roles are explicit: `UiMirror`, `LiveWorker`, and
  `OfflineAnalysis`. Roles are assigned before copied settings are applied.
  Hardware plugins must perform effects only when both the role is
  `LiveWorker` and `ExecutionContext::hardware_effects_allowed()` is true.
- Live-effect revocation is synchronously acknowledged by the worker on
  replay/mode transitions and shutdown.

Atomic domain operations, validation, leases, and safe-state behaviour belong
to the target service. The host remains domain-neutral and does not proxy
individual settings.

## Consequences

The modulation and photodiode plugins can remain the sole Teensy owners while
workflow plugins coordinate them without opening serial ports. Control works
on a camera-less bench, including host-rendered owner telemetry; identity
spoofing is prevented by the host, and GUI or offline mirrors stay inert.
Plugin authors must rebuild for ABI v6 and must design explicit semantic
service contracts rather than exposing arbitrary setting mutation.

## References

- ADR 024: Host/Worker Plugin State Ownership
- ADR 026: External Triggers and Execution Context
- ADR 028: Allowlisted Plugin to Host Commands
- `docs/features/plugin-service-control-plane.md`
