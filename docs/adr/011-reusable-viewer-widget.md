# ADR 011: Reusable Viewer Widget For Embedded And Popup Hosts

## Status

Accepted

## Context

ADR 005 introduced a shared preview workspace so the embedded preview and popup would not drift in
zoom, crop, and 2D/3D state. ADR 010 then added viewer tools and external preview bridges on top
of that shared workspace.

The next problem was structural: the center panel in `app.rs` had grown into one large block that
mixed heading text, toolbars, canvas logic, replay controls, lower control panels, popup behavior,
and host-side side effects. The popup still rendered a reduced texture-only fallback, so new
viewer features had to be implemented twice or were simply unavailable in the popup.

The reconstruction-window follow-up also needs the same viewer surface without copying the entire
center panel again.

## Decision

Extract the full central viewer panel into `augur-gui/src/viewer_widget.rs`.

- `ViewerState` owns viewer-local mutable state such as view mode, zoom/pan, tools, contrast,
  annotations, scale bar, and auxiliary windows
- `ViewerInput` carries host-owned per-frame data such as textures, frames, stats, warnings,
  replay status, and external-stream placeholders
- `ViewerOutput` reports side-effect requests back to the host, including ROI commits, popup
  toggles, replay transport actions, and rerender triggers
- `CameraApp` remains the owner of pipeline lifecycle, config mutation, replay controllers,
  texture generation, external-tool connections, and plugin execution
- when the popup opens, the active `ViewerState` moves into popup-shared data so the popup hosts
  the real viewer instead of a reduced texture renderer; closing the popup moves that same state
  back into the main host

## Consequences

### Positive

- the popup now exposes the same viewer surface as the main window, including replay controls and
  lower viewer controls
- future hosts such as the reconstruction window can wrap the same viewer component instead of
  copying central-panel code again
- `app.rs` regains a cleaner boundary: it orchestrates side effects while `viewer_widget.rs` owns
  the viewer UI tree
- viewer-local state transfer between hosts preserves zoom, active tools, annotations, contrast,
  and replay drag state

### Negative

- the popup path now carries a larger shared-data structure because it needs the same inputs as the
  embedded viewer
- rerender-sensitive settings such as preview mode and histogram contrast require a feedback path
  from popup UI actions back into host-owned texture generation
- the extracted viewer API introduces another internal module boundary contributors need to learn

## Alternatives Considered

### Keep separate embedded and popup renderers

Rejected because the popup was already lagging behind the main viewer feature set, and every future
viewer change would continue to duplicate effort or regress popup behavior.

### Move viewer side effects into the widget module

Rejected because replay controllers, config writes, plugin execution, and external-tool lifecycle
belong to the host app, not to a reusable UI module.
