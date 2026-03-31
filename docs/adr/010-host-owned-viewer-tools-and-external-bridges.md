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
- the first bridge, ImageJ/Fiji, uses a bounded background sender on the Rust side so external
  streaming cannot backpressure capture or preview processing, while still preserving short bursts
  of frame history for downstream tools
- the ImageJ/Fiji bridge is paired with a tiny bundled `AugurBridge_.jar` plugin that provides a
  loopback-only TCP listener on port `57294` with a binary frame protocol
  (`frame <w> <h> <scale> <seq> <timestamp_us>\n` + raw u16 LE pixels)
- the default ImageJ/Fiji presentation is a capped in-memory `ImageStack` with auto-follow on the
  newest slice, bounded history trimming, and archived-stack rollover on dimension changes; the
  plugin also keeps a live-only fallback mode for users who only want the old single-frame preview
- ImageJ-side frame display updates are batched onto the EDT so the bridge can absorb bursty input
  without one blocking EDT round-trip per frame

## Consequences

### Positive

- Augur gains useful inspection tools without pushing microscopy-specific logic into `augur-core`
- plugin overlays stay focused on analysis output rather than ad hoc GUI interaction state
- the external-viewer handoff has a clear host-owned extension point for future backends
- ImageJ/Fiji streaming can be enabled or disconnected without changing the capture pipeline split
- the ImageJ/Fiji workaround stays small and isolated to one auxiliary plugin artifact rather than
  pulling Java-specific remoting code into the Rust bridge
- researchers can now use native ImageJ stack tools such as scrubbing, temporal measurements, and
  stack projections on recent preview history without writing intermediary TIFF files

### Negative

- `augur-gui` owns more UI state and canvas interaction logic than before
- the embedded preview and popup still do not share every interactive overlay path perfectly, so
  future polish may further reduce divergence there
- the ImageJ bridge is intentionally best-effort and now requires a one-time plugin install inside
  ImageJ/Fiji, plus manual validation against local Fiji bridge setups
- the new timeline mode moves bounded frame-history memory pressure into ImageJ/Fiji itself, so the
  plugin must keep an explicit frame cap and users still need to size it sensibly for their image
  dimensions

## Alternatives Considered

### Model viewer tools as analysis plugins

Rejected because user-driven measurements, annotations, and display controls are not analysis
outputs, and forcing them through the plugin `Overlay` path would couple transient UI interactions
to frame-rendered texture data.

### Push external streaming into `augur-core`

Rejected because external-viewer handoff is a GUI-host concern with connection state, temporary file
management, and lossy delivery semantics. The capture/runtime boundary in `augur-core` should remain
generic and viewer-agnostic.
