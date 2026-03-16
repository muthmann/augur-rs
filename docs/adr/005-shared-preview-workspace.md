# ADR 005: Shared Preview Workspace State for 2D/3D GUI Views

## Status

Accepted

## Context

`augur-gui` started with a single passive preview image plus toolbar-level side-panel toggles. The current UI now needs multiple coordinated preview surfaces and interaction modes:

- a richer 2D preview with cursor coordinates, ROI drag selection, zoom/pan, and ROI cropping
- an enlarged popup that should behave exactly like the embedded preview instead of becoming a second divergent implementation
- an alternative 3D point-cloud view backed by recent raw events

Implementing each surface independently would duplicate coordinate transforms, zoom state, ROI handling, and raw-event buffering decisions across several `egui` widgets. The 3D mode also needs raw `CdEvent`s, but the existing pipeline contract should stay unchanged for normal 2D preview.

## Decision

Adopt a shared preview workspace state inside `augur-gui` and keep the point-cloud implementation local to the GUI crate:

- `CameraApp` owns one preview workspace state that tracks view mode, zoom/pan, crop mode, hover coordinates, ROI drag state, popup visibility, and point-cloud state
- both the center panel and the enlarged popup reuse the same 2D preview controls and viewport math
- the 3D view lives in a dedicated `point_cloud.rs` module that keeps a bounded recent-event history and renders with egui's painter API
- the GUI requests raw events from the pipeline only when the 3D view is active or an enabled plugin already requires raw events
- side-panel collapse toggles move to the panel edges, while the top toolbar is reserved for session actions and the 2D/3D view switch

## Consequences

### Positive

- 2D preview behavior stays consistent between the embedded view and the popup
- ROI selection, zoom, crop, and hover coordinates share one coordinate system and one source of truth
- the 3D point-cloud feature does not require a new rendering dependency or a broader pipeline redesign
- raw-event capture remains opt-in instead of becoming an unconditional preview cost

### Negative

- `augur-gui/src/app.rs` now owns more UI state than before, even though the point-cloud math was split into its own module
- the 3D view quality depends on bounded history and point-limit defaults, so follow-up tuning may still be needed after interactive use
- replay sessions only have point-cloud data for frames captured while raw-event delivery is enabled

## Alternatives Considered

### Separate state per preview surface

Rejected because popup and embedded-preview drift would be very likely, especially around ROI math and zoom/crop behavior.

### Always attach raw events to preview frames

Rejected because normal 2D preview does not need the extra memory and copy cost, and the existing plugin-driven `raw_events_needed` mechanism already provides the right opt-in boundary.
