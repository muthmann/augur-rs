# Plugin Service Control Plane

## Summary

Dynamic plugin ABI v6 adds a worker-owned, frame-independent control plane.
It lets workflow plugins orchestrate existing device-owner plugins without
opening the same hardware themselves. It also provides narrowly scoped camera
recording commands with definitive start and finalization receipts.

## Contract

Control-plane plugins declare a stable `id` in `plugin.toml`. IDs use lowercase
ASCII letters/digits plus `.`, `-`, and `_`, and must be unique. A plugin may
also declare `host_commands = ["start_recording", "stop_recording"]`.

The host assigns one of three runtime roles before applying settings:

- `UiMirror`: settings UI only; never perform hardware effects.
- `LiveWorker`: authoritative live instance and sole effectful owner.
- `OfflineAnalysis`: deterministic analysis; never perform hardware effects.

`process_control` runs on the live worker every 50 ms with replies and the
latest revisioned peer snapshots. It may emit semantic service requests or
declared host commands. `handle_service_request` runs on the target's same
worker instance. Requests are deduplicated by source plugin ID and request ID.

Request IDs belong to the plugin instance and restart at 1 when that instance
is recreated, so every dedupe entry is cleared on plugin reload; a
discontinuity retires only settled entries, keeping requests still awaiting a
reply. The maps are bounded and evict oldest-answered-first, and reply inboxes
addressed to disabled or unloaded plugins are pruned each tick.

The same periodic result carries the live worker's host-view registries and
generation-cached datasets. This is required for device-owned telemetry such
as a photodiode trace: it must remain visible when the camera produces no
preview frames. Analysis and control results have one worker-local monotonic
snapshot sequence, so two independent result channels cannot replace a newer
dataset with an older one. A busy analysis queue also checks the 50 ms control
deadline after processing commands instead of starving control publication.

## Recording commands

`StartRecording` accepts a run ID, relative base path, and metadata. The path
is confined below the host's configured output directory, uses create-new
semantics, and returns the actual path and UTC start time. `StopRecording`
finalizes through the normal pipeline and returns byte size, SHA-256, duration,
and actual path. Hashing runs off the GUI thread. For a plugin-owned recording
that started from Preview and was stopped **by that plugin**, the host restores
Preview before delivering the final receipt, so the workflow may immediately
start another run. Manual and pipeline-error stops also produce a terminal
receipt on the start request ID — the only channel back to the plugin, since
the inbox carries no unsolicited host events — but deliberately do **not**
restore Preview, so the host never re-opens a device the operator just closed
or that just failed. The reply cache is write-once, so that end-of-run
notification cannot displace the cached `RecordingStarted`. Failures return a
typed partial receipt with the actual path and all available file facts.

Host commands are routed regardless of the analysis epoch: both starting and
stopping a recording bump the epoch, so a plugin-driven cycle reliably produces
in-flight control results carrying the superseded epoch, and dropping those
would leave the plugin waiting with no reply and no timeout.

## Safety and ownership

`ExecutionContext` is the final authorization gate. The host synchronously
revokes live effects before mode transitions complete and during worker
shutdown. The GUI renders worker-owned status entries, so an inert mirror does
not open a serial device merely to report status.

For Stage-A, the intended boundary is: modulation and photodiode plugins own
their Teensy connections; an A1 workflow sends their semantic commands and
reads their snapshots. Stage-A payload schemas remain outside the generic host.

## Verification

Regression coverage includes ABI/message serialization, stable manifest ID
validation, no-frame periodic control and host-view publication, cross-channel
snapshot ordering, synchronously acknowledged effect revocation, and the
existing core/runtime/GUI suites.
