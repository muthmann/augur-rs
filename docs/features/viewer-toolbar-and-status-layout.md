# Viewer Toolbar And Status Layout

## Summary

`augur-gui` now uses the same tiered viewer layout for both the shared 2D preview and the host
3D investigation pane:

- a top toolbar for tools, navigation, histogram/popup actions, and 3D camera presets
- a collapsible `Display` strip for rendering-specific controls
- the viewport itself
- a compact footer for live status plus expandable diagnostics

This keeps frequently used navigation controls visible while moving lower-frequency rendering
settings and verbose diagnostics out of the main tool row.

## Behavior

- The 2D viewer toolbar keeps pointer/ROI/measurement/annotation tools, zoom controls, crop,
  histogram, and popup actions.
- The 2D `Display` strip now owns:
  - preview mode selection
  - scale-bar visibility and position
  - time-surface decay tuning
  - annotation list and selected-annotation statistics
- The 2D footer now shows a one-line summary with preview throughput, frame ON/OFF balance, hover
  readout, and ruler measurements. Expanded diagnostics keep pipeline stats, runtime-dirty state,
  analysis warnings, notices, hotpixel masking actions, replay-open progress, and errors.
- The 3D toolbar now keeps only view navigation actions: reset, fit, focus-selected, and the XY /
  XZ / YZ / ISO camera presets.
- The 3D `Display` strip now owns:
  - depth-slice enable + thickness
  - point scale
  - raw-event history range
  - raw-event point budget
- The 3D footer now summarizes visible layers, point count, retained history, and active focus
  volume, with expandable orientation and control guidance below.
- Both display strips stay open by default and remember their open/closed state in the respective
  viewer state structs.

## Files

| File | Role |
|---|---|
| `augur-gui/src/viewer_widget.rs` | shared 2D toolbar/display-strip/footer composition and diagnostics |
| `augur-gui/src/inspection_3d.rs` | 3D toolbar/display-strip/footer composition and retained-history status |

## Verification

```bash
cargo fmt --all
cargo check -p augur-gui --bin AugurRS
cargo clippy --workspace
cargo test --workspace
```
