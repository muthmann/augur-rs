# ADR 016: Host-Owned Generic Investigation Workspace

## Status

Accepted

## Context

The older GUI split 2D preview and 3D point-cloud inspection into separate view modes with weak
coordination:

- 3D inspection was tied to a software-rasterized raw-event point cloud
- table views had no stable host-owned identity for linked selection
- plugin overlays were the main interaction surface even though they are only one possible visual
  output
- view coordination depended on transient row positions and plugin-local conventions more than on
  durable host metadata

That made the GUI less trustworthy for replay-first investigation work. Researchers need one
coherent host-owned state model that explains what is selected, filtered, visible, and stale.

## Decision

Adopt a generic investigation workspace architecture inside `augur-gui`.

- Add a host-owned `InvestigationState` that tracks:
  - layout
  - active ROI
  - selected and hovered rows
  - focused layers
  - 2D/3D ROI linking
  - camera focus target
  - table sort state
  - layer visibility/style state
- Replace the old software-rasterized 3D inspection surface with an offscreen WGPU point renderer.
- Remove the legacy viewer-local 3D mode so `InvestigationLayout` is the only 2D/3D layout
  authority in the host.
- Keep the existing 2D preview widget, but paint host-owned linked markers on top of it.
- Treat 2D preview, 3D inspection, and host-rendered tables as linked views over the same
  investigation state.
- Key linked selection by stable row ids when plugins provide them, with host-local generated ids
  as a fallback only.
- Key styling and visibility by host-owned layer ids instead of plugin names.
- Extend `augur-plugin-api` additively with generic dataset/view/display metadata so plugins can opt
  into better linking without forcing domain-specific host logic.
- Extend the generic overlay contract with a richer marker-collection payload so 2D overlays can
  carry shape, size, timestamp, stable-id, dataset-id, and layer/source metadata without turning
  overlays back into the primary host state model.
- Reuse the same layout model for the main viewer and popup viewer.
- Prefer immediate current-frame reruns for paused replay parameter tuning, while keeping broader
  recompute scopes explicit follow-up work.

## Consequences

### Positive

- The host now owns view coordination instead of relying on painter overlays alone.
- Linked selection is more durable across sort/filter/regeneration because it prefers stable row
  ids.
- The 3D surface is generic and layer-based instead of encoding domain-specific semantics.
- Popup and embedded viewing now share the same investigation layout model.
- The right-side inspector can expose provenance, visibility, and stale-state information in one
  place.
- Overlay markers can participate in linked 2D selection without forcing plugins to render
  custom host UI.
- The split view has one visible, draggable divider and one consistent control model across the
  main and popup viewers.

### Negative

- `augur-gui` now owns more interaction state and rendering code.
- The host-view system and popup viewer flow are more complex because they must keep table, 2D, and
  3D state synchronized.
- Early plugin ABI/API churn is more likely while the generic investigation metadata and overlay
  callback table settle.

## Alternatives Considered

### Keep extending plugin overlays as the primary interaction layer

Rejected because overlays are only one rendering output and do not provide stable host-owned
selection, provenance, or cross-view linking by themselves.

### Keep row-index-based table selection

Rejected because row indices are unstable under filtering, sorting, replay regeneration, and
plugin recomputation.

### Introduce domain-specific 3D semantics in the host

Rejected because `augur-core`, `augur-gui`, and `augur-plugin-api` should remain generic host
layers rather than absorbing EVE- or SMLM-specific research semantics.
