# ADR 012: Generic Plugin Boundary With Companion Domain-Type Crates

## Status

Accepted

## Context

The runtime plugin system had already moved to a flat `PluginVTable`, host-owned event history,
host-owned global settings, and a host-view registry. Two problems still remained:

1. `augur-plugin-api` still carried domain-specific localization/SMLM payloads in its core API
   crate.
2. `augur-gui` still kept one reconstruction-specific host path alongside the generic host-view
   system.

That left the plugin surface partially generic in structure but still domain-specific in meaning.

## Decision

Commit fully to a generic core plugin boundary.

### Core contract

- keep `augur-plugin-api` focused on generic host/runtime contracts only
- move optional domain payloads into companion crates such as `augur-plugin-types`
- treat the current work as a pre-v1 breaking change with no compatibility shim

### Host-owned UI

- remove the reconstruction-specific runtime hook and reconstruction-specific GUI pipeline
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

- the runtime ABI changes again and all external plugins must rebuild
- plugin authors now manage one more concept (`PluginCapabilities`)
- some host features that were previously hard-coded must now be expressed as dataset/view recipes

## Alternatives Considered

### Keep Localization Types In `augur-plugin-api`

Rejected because it keeps the core plugin boundary semantically tied to one analysis domain.

### Keep A Dedicated Reconstruction Window Beside Host Views

Rejected because it duplicates host logic and invites more domain-specific escape hatches over
time.

### Make Plugins Render Their Own `egui`

Rejected because the goal is to keep `augur-gui` generic, testable, and stable while still letting
plugins expose rich structured outputs.
