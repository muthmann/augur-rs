# ADR 012: Generic Plugin Boundary With Companion Domain-Type Crates

## Status

Accepted

## Context

The runtime plugin system uses a flat `PluginVTable`, host-owned event history, host-owned global settings, and a host-view registry. Two design goals drive this ADR:

1. `augur-plugin-api` should contain only generic host/runtime contracts, not domain-specific payload types.
2. `augur-gui` should use the generic host-view system for all plugin output, without domain-specific host paths.

This keeps the plugin surface generic in both structure and meaning.

## Decision

Commit fully to a generic core plugin boundary.

### Core contract

- keep `augur-plugin-api` focused on generic host/runtime contracts only
- move optional domain payloads into companion crates such as `augur-plugin-types`
- no compatibility shims for domain-specific types in the core crate

### Host-owned UI

- route all plugin analysis UI through the host-view registry
- require plugin-owned analysis UI to flow through the host-view registry
- keep views declarative and host-rendered instead of allowing plugin-owned `egui`

### Runtime cost control

- make retained event history explicit through `PluginCapabilities`
- only retain frame history when an enabled plugin opts in
- do not store empty decoded-event frames

## Consequences

### Positive

- the core SDK boundary is cleaner and easier to reason about
- future plugins from unrelated domains can reuse the same host-view and context mechanisms
- domain payloads can evolve in companion crates without making the core API semantically narrow
- runtime cost becomes easier to predict because expensive retained history is explicit

### Negative

- external plugins must rebuild against the current `augur-plugin-api`
- plugin authors now manage one more concept (`PluginCapabilities`)
- host features must be expressed as dataset/view recipes rather than hard-coded paths

## Alternatives Considered

### Keep Domain-Specific Types In `augur-plugin-api`

Rejected because it keeps the core plugin boundary semantically tied to one analysis domain.

### Keep A Dedicated Reconstruction Window Beside Host Views

Rejected because it duplicates host logic and invites more domain-specific escape hatches over
time.

### Make Plugins Render Their Own `egui`

Rejected because the goal is to keep `augur-gui` generic, testable, and stable while still letting
plugins expose rich structured outputs.
