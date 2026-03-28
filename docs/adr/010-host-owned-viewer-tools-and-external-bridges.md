# ADR 010: Host-Owned Viewer Tools And External Preview Bridges

## Status

Accepted

## Context

The shared preview workspace already gave `augur-gui` one coordinated place for zoom, crop, ROI
selection, and 2D/3D view switching. Researchers still needed two more capabilities:

- richer host-side image inspection tools such as histogram-driven contrast, measurements, and
  software annotations without leaving Augur
- a lightweight way to hand the live preview off to external viewers such as ImageJ/Fiji without
  turning those workflows into analysis plugins

Treating these needs as analysis plugins would blur an important boundary. Histogram controls,
measurements, and annotations are user-interaction features, not frame-processing outputs. Likewise,
an external viewer bridge needs lossy background delivery and connection state, not a new
`augur-core` contract.

The initial ImageJ/Fiji implementation also hit an upstream compatibility limit: modern ImageJ no
longer ships the historic raw TCP `SocketListener`, so a direct Rust socket client cannot talk to a
stock ImageJ/Fiji install without an additional compatibility layer.

## Decision

Keep viewer tools and external preview bridges host-owned inside `augur-gui`.

- viewer tools live in a dedicated `viewer_tools/` module tree
- interactive viewer overlays are painted with `egui::Painter` on top of the preview canvas instead
  of extending the analysis `Overlay` enum
- preview display state is promoted from one percentile slider to explicit display settings
  (`min`, `max`, `gamma`, `colormap`)
- external viewers implement a small `ExternalTool` trait under `external_tools/`
- the first bridge, ImageJ/Fiji, uses a bounded background sender so external streaming stays
  lossy/latest-oriented and cannot backpressure capture or preview processing
- the ImageJ/Fiji bridge is paired with a tiny bundled `AugurBridge.jar` plugin that restores a
  loopback-only TCP `eval` listener on port `57294`, letting Augur keep the simple write-only
  protocol instead of implementing Java RMI/serialization in Rust

## Consequences

### Positive

- Augur gains useful inspection tools without pushing microscopy-specific logic into `augur-core`
- plugin overlays stay focused on analysis output rather than ad hoc GUI interaction state
- the external-viewer handoff has a clear host-owned extension point for future backends
- ImageJ/Fiji streaming can be enabled or disconnected without changing the capture pipeline split
- the ImageJ/Fiji workaround stays small and isolated to one auxiliary plugin artifact rather than
  pulling Java-specific remoting code into the Rust bridge

### Negative

- `augur-gui` owns more UI state and canvas interaction logic than before
- the embedded preview and popup still do not share every interactive overlay path perfectly, so
  future polish may further reduce divergence there
- the ImageJ bridge is intentionally best-effort and now requires a one-time plugin install inside
  ImageJ/Fiji, plus manual validation against local Fiji bridge setups

## Alternatives Considered

### Model viewer tools as analysis plugins

Rejected because user-driven measurements, annotations, and display controls are not analysis
outputs, and forcing them through the plugin `Overlay` path would couple transient UI interactions
to frame-rendered texture data.

### Push external streaming into `augur-core`

Rejected because external-viewer handoff is a GUI-host concern with connection state, temporary file
management, and lossy delivery semantics. The capture/runtime boundary in `augur-core` should remain
generic and viewer-agnostic.
