# ADR 006: Host-Owned Dataset/View Registry for Plugin Outputs

## Status

Accepted

## Context

The first dynamic plugin integration added one reconstruction-specific host hook:
`Plugin::accumulated_localizations()`. That solved the immediate reconstruction window use case,
but it tightly coupled the GUI shell to a single scientific payload and view.

The GUI now needs a more general way for built-in and runtime plugins to expose structured,
host-rendered datasets without pushing domain-specific UI into `augur-gui` or extending the legacy
FFI surface one ad hoc field at a time.

## Decision

Adopt a host-owned dataset/view registry with a versioned ABI v2 entrypoint.

The new model splits plugin-facing contributions into:

- dataset descriptors, which declare stable ids, titles, kinds, and empty-state messages
- view descriptors, which declare stable ids, placements, and host-rendered view kinds
- on-demand dataset payload fetches keyed by dataset id

Runtime plugins now export both:

- the legacy `augur_plugin_vtable` symbol for backward compatibility
- a new `augur_plugin_entry_v2` descriptor that points to a `PluginVTableV2`

The host prefers ABI v2 when available and falls back to the legacy vtable only when the new
symbol is absent.

Descriptor conflicts are resolved in existing plugin execution order:

- later providers override earlier ones only when descriptor metadata matches exactly
- conflicting duplicate ids are ignored and logged
- dataset payloads are fetched lazily, only when a visible host panel or window needs them

## Consequences

### Positive

- `augur-gui` can host generic tables and density views without plugin-specific UI code
- built-in and runtime plugins now share one host-view path
- legacy plugins keep loading unchanged
- plugin outputs stay machine-readable and easier to migrate across repositories

### Negative

- the host now owns more UI state: resolved registries, dataset caches, and per-view render state
- plugin authors must keep descriptor metadata stable when intentionally sharing ids
- the ABI surface is slightly larger and requires explicit version validation

## Alternatives Considered

### Keep Adding Reconstruction-Specific Hooks

Rejected because it would keep coupling the host shell to one analysis domain at a time.

### Move Analysis Rendering Into Plugins

Rejected because the design goal is to keep `augur-gui` as a general-purpose recorder and plugin
host, while detailed science implementations continue moving into companion repositories.
