# ADR 028: Allowlisted Plugin to Host Recording Commands

## Status

Accepted (2026-07-20), implemented for recording start/stop in ABI v6.

## Context

A workflow plugin may need camera capture to start and stop with a laboratory
protocol. Dynamic plugins must not receive an ambient file-system or general
GUI-control capability.

## Decision

The ABI exposes a closed `HostCommand` enum. The implemented verbs are
`start_recording`, `stop_recording` and `apply_biases` — the last added by
ADR 036, which keeps the same declaration, dedupe and rejection machinery
described here.

- A plugin must declare each verb in `plugin.toml` via `host_commands`. The
  live worker rejects undeclared commands before they reach the GUI.
- Commands are accepted only while the worker has a live execution context
  with effects enabled. Replay, offline analysis, and GUI mirrors fail closed.
- Requests use idempotent request IDs. The runtime emits a request once and
  caches the GUI reply for exact retries. The cache is **write-once**: the
  first reply for a request ID wins, so the end-of-run notification described
  below cannot displace a cached `RecordingStarted`.
- Request IDs are owned by the plugin instance and restart at 1 when that
  instance is recreated, so the runtime clears every dedupe entry on plugin
  reload. A discontinuity retires only *settled* entries — a request still
  awaiting its reply is kept, because dropping it would strand the plugin with
  no reply and no timeout. Both maps are bounded (oldest-answered-first
  eviction) so a long session cannot grow them without limit.
- Host commands are routed to the GUI regardless of the current analysis
  epoch. Starting and stopping a recording both bump the epoch, so a
  plugin-driven start/stop cycle reliably produces in-flight control results
  carrying the superseded epoch; dropping those would deadlock the plugin.
  Individual command handlers validate host state, so a stale command is
  rejected rather than misapplied.
- `StartRecording` supplies a run ID, a **relative** base path, and string
  metadata. The host confines the path below the directory of its configured
  output path, appends `.raw` when absent, and rejects absolute paths,
  traversal, and existing targets. The recorder uses `create_new` to close the
  check/create race. Plugin metadata is namespaced in recording metadata.
- A successful start returns the actual absolute raw path and UTC start time.
  The GUI visibly identifies the controlling plugin while recording.
- Only the plugin that started a plugin-owned recording may stop it. Shutdown
  uses the normal pipeline path; SHA-256 is computed off the GUI thread. The
  final reply contains actual path, byte size, SHA-256, and recorded duration.
- After a **plugin-requested** stop finalizes cleanly, the host restores live
  Preview before returning the final receipt. This makes the receipt a reliable
  boundary for the next workflow recording instead of leaving the host idle.
  An operator Stop or a pipeline failure does **not** restore Preview — the
  host must not re-open a device the user just closed or that just failed.
- Human intervention remains authoritative. A manual or pipeline-error stop
  uses the same finalization path and sends a terminal reply on the original
  start request ID, since `PluginControlInbox` carries no unsolicited host
  events and that ID is the only channel back to the plugin. If shutdown or
  hashing failed, `RecordingPartial` still reports the actual path, available
  size/hash, duration, and reason.

Absolute paths selected inside a plugin are intentionally not trusted. A
future host-owned, persisted directory grant may widen the allowed roots; a
manifest declaration alone is insufficient authority.

## Consequences

Workflow plugins can coordinate capture without direct camera or arbitrary
file-system access. They must choose names relative to the host-configured
recording directory and handle explicit rejection/finalization replies.

## References

- ADR 013: Self-Describing Recording Metadata
- ADR 026: External Triggers and Execution Context
- ADR 027: Worker-Owned Semantic Plugin Services
- `docs/features/plugin-service-control-plane.md`
