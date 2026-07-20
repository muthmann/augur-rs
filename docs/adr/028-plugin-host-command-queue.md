# ADR 028: Allowlisted Plugin → Host Command Queue

## Status

Proposed (2026-07-17). Not implemented — recorded so the security-sensitive
shape is agreed before any code lands.

## Context

Researchers want a plugin to drive the host application itself: start/stop a
recording, set the output filename, begin an analysis run. A protocol
executor (e.g. the `stage-a-modulation` protocol runner) that could also
arm a camera recording at step 0 and stop it at the last step would let one
"Run" button capture a complete, correlated experiment.

Today this is deliberately impossible:

- The plugin `ExecutionContext` is **fail-closed** (ADR 026): plugins
  observe host state but cannot command it.
- `HostActionRequest` (ADR 018) is host → plugin only.
- There is no reverse channel, and adding an open one would let any loaded
  dynamic library (`libloading`, ADR 003) drive recordings and write files
  under the host's authority — an unacceptable ambient capability.

## Decision

Add a **narrow, allowlisted, host-owned command queue** in the plugin → host
direction, mirroring the ADR 018 action bus in reverse.

- Define a closed enum of host commands the host knows how to execute, e.g.
  `HostCommand::StartRecording { name: Option<String> }`,
  `HostCommand::StopRecording`, `HostCommand::StartAnalysisRun { .. }`. The
  set is **fixed in the host** — plugins choose from it, never extend it.
- Plugins enqueue `HostCommandRequest { request_id, command }`; the host
  drains the queue each GUI tick and executes on the UI thread through the
  same code paths the menu items use (so recording still respects overwrite
  protection, timestamping, ADR 013 metadata, etc.).
- **Per-plugin capability gate.** A plugin must declare each verb it may use
  in its manifest (`host_commands = ["start_recording", "stop_recording"]`),
  and the user must grant it (a one-time consent, stored per plugin). A
  plugin with no declared/consented verbs cannot enqueue anything — the
  fail-closed default of ADR 026 is preserved.
- **Every executed command is visible**: a toast + log entry naming the
  plugin and the command, and a persistent indicator while a
  plugin-initiated recording is active (so a human always knows the plugin,
  not they, started it).
- Commands are **best-effort and rejectable**: the host may refuse (already
  recording, no camera, invalid path) and reports the rejection back to the
  plugin via the same `request_id`. Plugins must tolerate refusal.

## Consequences

### Positive

- Enables the one-button correlated-capture workflow without opening a
  general host-control surface.
- The closed verb set + per-plugin consent keeps the blast radius small and
  auditable; the default stays fail-closed.
- Reuses existing host execution paths, so file-safety and metadata
  invariants are automatically honoured.

### Negative / Risks

- Any plugin → host capability is a privilege escalation for a dynamically
  loaded binary. The consent gate and closed verb set are load-bearing;
  they must not be bypassable by manifest edits alone (consent is stored
  host-side, keyed to the plugin identity/manifest hash).
- Filename control (`name`) is the sharpest edge: it must be sanitised and
  confined to the configured data directory, never an arbitrary path.

### Neutral

- Camera recording implies frames are flowing, so command execution can ride
  the existing worker/GUI update path; no new threading model is needed.

## References

- ADR 003: Dynamic Plugin Loading via C FFI and `libloading`
- ADR 013: Self-Describing Recording Metadata
- ADR 018: Host Action Bus For Plugin-Declared Actions
- ADR 026: External-Trigger Delivery And Plugin Execution Context (ABI v5)
- ADR 027: Host-Routed Plugin Setting Requests
- `docs/features/photodiode-lab-workflow-design.md` (§7)
