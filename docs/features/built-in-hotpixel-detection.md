# Built-In Hotpixel Detection

## Summary

`augur-gui` now ships a host-owned hotpixel detector as a core analysis tool.
It no longer depends on an in-repo or installed plugin to detect persistent hot
pixels, highlight them on the preview, or copy those detections into the IMX636
hardware DEM mask.

## Behavior

- The detector runs on every processed live-preview and replay frame before any
  runtime plugin executes.
- Settings are host-owned and session-local:
  - `enabled`
  - `Smoothing depth`
  - `Threshold factor`
  - `Min absolute count`
- Detection uses an exponential moving average of per-pixel event counts to
  suppress flicker and emphasize persistent hot pixels.
- Results appear as generic highlight overlays and analysis warnings so the
  existing preview rendering path stays reusable.
- `Mask detected hotpixels` now reads from the detector's explicit built-in
  state instead of scanning arbitrary `HighlightPixels` overlays. Runtime
  plugins can no longer trigger DEM-mask copies accidentally.

## Architecture Notes

- `augur-core` no longer owns hotpixel-detection logic or ROI-grid analysis.
- `augur-gui` owns the built-in hotpixel feature directly in its host-side UI
  and analysis flow.
- The runtime plugin host remains in this repository, but plugin
  implementations are expected to live outside it.

## Files

| File | Role |
|---|---|
| `augur-gui/src/hotpixel.rs` | built-in detector state, controls, processing, and unit tests |
| `augur-gui/src/app.rs` | analysis execution order, Analysis-panel wiring, and DEM-copy flow |
| `augur-gui/src/viewer_widget.rs` | viewer-side `Mask detected hotpixels` action |
| `augur-core/src/analysis/mod.rs` | generic overlay and warning data types consumed by the GUI and runtime host |

## Verification

- `cargo check --workspace`
- `cargo test -p augur-gui hotpixel`
- `cargo test -p augur-gui viewer`
