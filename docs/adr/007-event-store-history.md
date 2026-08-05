# ADR 007: Host-Owned EventStore for Plugin History

## Status

Accepted

Planned successor: ADR 020, `Unified Upstream Event Source`, is Proposed and
will supersede this decision once the plugin-runtime cutover is accepted. The
GUI host has already moved its runtime retained history to a projection over
upstream event ranges; the public `EventStoreHandle` contract remains in force
until ADR 020 is accepted.

## Context

Runtime plugins increasingly need more than the current frame:

- sliding-window event analysis
- cross-frame correlation and accumulation
- lightweight state that survives between frames without inventing custom host hooks

Letting every plugin buffer its own event history would duplicate memory use, duplicate decoding
work, and make retention behavior inconsistent across plugins.

## Decision

Adopt a host-owned `EventStore` inside `augur-gui` and expose it through
`augur-plugin-api::EventStoreHandle`.

The host:

- converts decoded events into FFI form once per frame
- appends them to a memory-budgeted retained history
- passes a read-only history handle into every dynamic plugin call
- keeps two JSON context scopes:
  - per-frame context, cleared on each frame
  - persistent context, cleared on analysis reset / topology reset

The retained frame deque is ordered by window, because
`frame_range_for_timestamps` binary searches it and plugins walk the returned
index range as a timeline. That ordering is enforced by the store itself, not by
its callers: attaching a different upstream source drops the previous source's
frames, and a frame whose window opens before the newest retained one drops the
pre-jump history. Host discontinuity commands remain the primary mechanism —
they also reset plugin state — but a missed command can no longer corrupt the
deque. Range queries run on the plugin FFI callback path, where a panic crosses
an `extern "C"` boundary and aborts the process, so they must never assert the
invariant they depend on.

This ships together with the v0.2 plugin API cleanup:

- one flat `PluginVTable`
- one exported entry symbol
- no ABI-v1/v2 fallback split

## Consequences

### Positive

- plugins can query retained event history without owning duplicate buffers
- retention limits are explicit, host-controlled, and testable
- persistent cross-frame state is available without adding plugin-specific callbacks
- the plugin boundary stays small and uniform

### Negative

- `augur-gui` now owns additional runtime state and memory-budget UI
- changing the `process_frame` signature is a coordinated rebuild for all plugins
- retained history increases host memory usage when event-heavy plugins are enabled

## Alternatives Considered

### Per-Plugin History Buffers

Rejected because it duplicates memory and makes retention semantics inconsistent.

### Another Optional ABI Layer

Rejected because the host-view transition already created unnecessary complexity. The v0.2 cleanup
uses one flat ABI and asks plugins to recompile once instead.
