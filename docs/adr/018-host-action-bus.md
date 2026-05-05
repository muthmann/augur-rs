# ADR 018: Host Action Bus For Plugin-Declared Actions

## Status

Accepted

## Context

Stages 1–3 of the Investigation Trustworthiness Pass gave the host enough
metadata to render plugin-owned datasets consistently and link them across
tables, overlays, and 3D views. The remaining gap was interactivity: the
host could show a plugin's data, but there was no generic way for the user
to ask a plugin to do something in response to a selection.

Before this change:

- Plugins could not expose buttons scoped to a dataset, a row, or a cluster
  without per-plugin host branches.
- There was no shared mechanism for capturing action parameters in a
  schema-driven modal.
- Single-cluster re-fit, commit, and discard flows for the EVE pipeline
  needed hard-coded integration.

## Decision

Introduce a generic host action bus on top of the persistent context bus:

- Add `HostActionDescriptor { id, title, scope, param_schema }` and
  `HostActionScope { Dataset, Row, Cluster }` to the plugin API. Plugins
  declare actions additively in `HostViewRegistry.actions` (serde-default,
  skip-if-empty — ABI-compatible).
- Add `HostActionRequest { request_id, action_id, scope_payload, params }`
  and `HostActionRequestQueue` at the persistent context key
  `CTX_INVESTIGATION_ACTION_REQUESTS` (`augur.investigation.action_requests`).
- The host is the single writer. On Apply, it resolves the current
  selection into `HostActionScopePayload`, assigns a monotonic `request_id`,
  appends to the queue, and publishes. Plugins dedupe via a cached
  `last_consumed_action_request_id`.
- The host renders a modal from `param_schema` (a serialized
  `SettingsSchema` JSON payload). Modal params are carried in the request
  as `serde_json::Value` — they are per-request, not plugin state.
- Action buttons are rendered in the investigation inspector. Scope-based
  enable logic uses the current selection (`StableRowKey` set) and the
  resolved dataset cache to populate cluster group values.

## Consequences

### Positive

- One host rendering path covers every current and future plugin action.
- Plugins control their action surface declaratively; host learns nothing
  domain-specific.
- The bus is reusable: refit, commit, discard, and future per-row or
  per-cluster operations all ride the same wire format.
- Discarded requests have no effect on plugin pipeline output, so
  byte-identical-on-discard is straightforward to verify.

### Negative

- The queue is append-only from the plugin's perspective. Cleanup is not
  automatic; the host re-publishes the queue each tick and plugins dedupe
  by `request_id`. Acceptable for v1 — cleanup is a host-side concern.
- Param-schema JSON is plugin-owned and not validated by the host. Plugins
  must tolerate missing or malformed params.

### Neutral

- Scope resolution for `Cluster` requires reading the host view dataset
  cache to extract group values. The host already caches TableV1 datasets
  for rendering, so the lookup is free in the common case.

## References

- [Investigation Action Requests](../features/investigation-action-requests.md)
- ADR 006: Host-Owned Dataset/View Registry for Plugin Outputs
- ADR 016: Host-Owned Generic Investigation Workspace
- ADR 017: Declarative TableV1 Provenance And Display Metadata
