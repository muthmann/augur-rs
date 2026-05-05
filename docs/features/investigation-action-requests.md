# Investigation Action Requests

## Summary

Plugins can declare host-rendered action buttons scoped to a dataset, a
selected row, or a selected cluster. The host renders a button per
applicable action in the investigation inspector, resolves the current
selection into a concrete scope payload, and — if the action declares a
`param_schema` — captures parameters in a modal driven by that schema.
Applying an action publishes a `HostActionRequest` on the persistent
context bus; the plugin drains the queue on the next tick and runs the
request.

The first concrete consumer is the EVE fitting plugin's single-cluster
re-fit + commit/discard flow.

## Plugin Contract

Additive, ABI-compatible fields:

- `HostViewRegistry.actions: Vec<HostActionDescriptor>` — serde-default,
  skip-if-empty. Plugins list the actions they expose.
- `HostActionDescriptor { id, title, scope, param_schema }`
  - `scope: HostActionScope` is one of `Dataset { dataset_id }`,
    `Row { dataset_id }`, or
    `Cluster { dataset_id, group_column }`.
  - `param_schema: Option<serde_json::Value>` — a serialized
    `SettingsSchema`. Optional.

The persistent context key `CTX_INVESTIGATION_ACTION_REQUESTS`
(`augur.investigation.action_requests`) carries a
`HostActionRequestQueue { requests: Vec<HostActionRequest> }`. Each request
is `{ request_id, action_id, scope_payload, params }`:

- `request_id` is assigned by the host, monotonic per session.
- `scope_payload: HostActionScopePayload` mirrors the scope variant:
  `Dataset { dataset_id }`, `Row { dataset_id, row_id }`,
  `Cluster { dataset_id, group_column, group_value }`, or `None`.
- `params: serde_json::Value` is the modal-captured payload, keyed by
  `SettingItem.key`.
- For `Cluster` actions, the host also snapshots the currently selected
  scope rows into `params["__augur_cluster_rows"]` so plugins can replay
  the action against a durable row snapshot instead of relying on the
  next frame's transient buffers.

Plugins dedupe consumed requests by caching the highest `request_id` they
have handled.

## Host Rendering

- `ResolvedHostViewRegistry` now owns `actions: Vec<ResolvedHostAction>`
  and filters them by resolved dataset. Actions whose scope targets an
  unresolved dataset are dropped with a warning.
- The investigation inspector renders one button per resolved action.
  Scope-based enable logic requires:
  - `Dataset` — the scope's dataset is resolved. Always available.
  - `Row` — exactly one `StableRowKey` is selected on that dataset.
  - `Cluster` — ≥ 1 rows are selected on the scope's dataset and they all
    share the same `group_column` value.
- If the descriptor carries a `param_schema`, the host opens a modal
  rendered from the schema. Modal state is owned by the in-flight request,
  not by the plugin.
- Apply resolves the current selection to a `HostActionScopePayload`,
  assigns a `request_id`, snapshots the selected cluster rows when
  applicable, appends to the queue, and publishes to
  `persistent_context_data`. Cancel drops the modal.
- The queue is re-published at the start of every `run_analysis` tick so
  plugins always see pending requests.

## Byte-Identical Pipeline On Discard

Discard and cancel must not perturb the main pipeline output. Plugins that
consume a request and then drop it (discard) should not touch the data
they publish to non-preview context keys. The fitting plugin verifies this
with a targeted unit test:
`discard_clears_preview_without_touching_current_results`.

## Files

- `augur-plugin-api/src/context.rs` — `HostActionDescriptor`,
  `HostActionScope`, `HostActionRequest`, `HostActionRequestQueue`,
  `CTX_INVESTIGATION_ACTION_REQUESTS`.
- `augur-gui/src/host_views.rs` — `ResolvedHostAction`,
  `ResolvedHostViewRegistry.actions`.
- `augur-gui/src/app.rs` — action button rendering, modal, queue
  publishing.

## References

- ADR 018: Host Action Bus For Plugin-Declared Actions
- ADR 016: Host-Owned Generic Investigation Workspace
- ADR 017: Declarative TableV1 Provenance And Display Metadata
