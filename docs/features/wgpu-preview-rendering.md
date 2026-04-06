# WGPU Preview Rendering

## Summary

GPU-accelerated preview rendering for `augur-gui`.

- add preview-stage timing so the GUI can report an end-to-end accepted-frame total plus dequeue,
  analysis, histogram, line-profile, CPU-fallback render, texture stage/submit, and
  external-bridge costs
- refactor preview preparation around renderer-ready payloads instead of a mandatory CPU-colored
  `ColorImage`
- add dual-backend startup with `AUGUR_RENDERER=glow|wgpu|auto`
- prefer `wgpu` in `auto`, retry `glow` automatically on startup failure, and keep CPU preview
  fallback available even inside a successful `wgpu` app session
- move analysis overlay composition out of the preview image and into the shared viewer painter
- route both the embedded viewer and popup viewer through the same `PreviewRenderer` abstraction
- maintain cached total and signed histograms directly in the preview thread so the CPU fallback
  path does not rescan finished count frames
- use WGPU compute accumulation and GPU histogram generation for the count-based preview modes when
  raw preview events are available
- make the WGPU `TimeSurface` path fully GPU-owned for accumulation, decay rendering, histogram
  generation, and hover-value queries while preserving the CPU fallback path

Capture guarantees are unchanged: preview work remains lossy/latest-only, and the disk path does
not depend on GPU presentation.

## Runtime Behavior

- `AUGUR_RENDERER=auto` tries `wgpu` first, then retries `glow` if startup fails.
- `AUGUR_RENDERER=wgpu` forces the `eframe` WGPU backend.
- `AUGUR_RENDERER=glow` forces the legacy OpenGL backend.
- `wgpu` startup uses `Backends::PRIMARY | Backends::GL`, `HighPerformance`, and
  `desired_maximum_frame_latency = 1`.
- The `Settings -> Advanced` panel now reports:
  - requested renderer
  - active app renderer
  - active preview renderer
  - backend and adapter strings
  - rolling preview-stage timings with a `Reset timings` action
- The shared viewer stats line also reports preview-thread averages from `augur-core`:
  - decode
  - accumulation
  - raw-event copy
  - frame send
- WGPU count-mode previews (`Intensity`, `RedBlue`, `SignedCount`) now request raw preview events
  automatically so the renderer can accumulate those frames on the GPU.
- WGPU `TimeSurface` now keeps its per-pixel last-event ticks in persistent GPU storage and no
  longer uploads a full-frame CPU-owned tick texture on the happy path.
- The renderer/performance labels in that panel have hover descriptions so the benchmark stages can
  be interpreted in the same way as the main settings tooltips.
- `Frame total` is the primary glow-versus-wgpu comparison metric because it measures the accepted
  preview-frame hot path end to end after throttling has admitted a frame.
- `Texture stage/submit` is intentionally narrower: it measures CPU-side staging and render
  submission only, and does not wait for GPU completion.

## Preview Pipeline

- `augur-gui/src/preview.rs` now exposes `PreparedPreviewFrame` variants:
  - `IntensityR16`
  - `PolarityRg16`
  - `TimeSurfaceR8`
- `augur-core` preview frames now carry cached total and signed-count histograms, so the GUI can
  reuse incremental upstream histogram work instead of rescanning full pixel planes for the common
  intensity, red/blue, and signed-count modes.
- The CPU fallback renderer reuses a persistent `ColorImage` plus a cached brightness LUT, so the
  inner loop no longer recalculates `powf` per pixel or allocates a fresh staging image every
  frame.
- Time-surface state is stored as quantized timestamp ticks (`64 µs`), which lets the WGPU path
  keep a compact last-tick surface on the GPU instead of forcing the CPU to regenerate and upload
  a full-frame decay image or tick texture on every frame.
- Histogram recomputation is gated to the modes that actually need it:
  - auto-contrast
  - the histogram window being open
- Auto-contrast no longer forces full histogram materialization when the histogram window is
  closed; it can derive the display max directly from the cached histogram path.
- The time-surface histogram now reuses the cached histogram built during time-surface decay
  preparation instead of rescanning the prepared image values a second time.
- In the WGPU path, `TimeSurface` histogram requests and auto-contrast sampling run against the
  GPU-owned tick surface and only read back the small histogram buffer.
- Line-profile recomputation is gated to an active line plus either the line-profile window or the
  line tool itself.
- Analysis overlays are painted in `augur-gui/src/viewer_widget.rs`, which keeps the preview
  texture image-only and backend-neutral.

## WGPU Presentation

- `augur-gui/src/preview_renderer.rs` introduces a shared `PreviewRenderer` abstraction with:
  - `CpuPreviewRenderer`
  - `WgpuPreviewRenderer`
- The WGPU path uploads compact source textures and renders into one offscreen `RGBA8` display
  texture registered with egui.
- Phase-4 source formats are:
  - intensity: `R16Uint`
  - red/blue and signed count: `Rg16Uint`
  - time surface fallback texture: `R32Uint` timestamp ticks for the CPU compatibility path only
- A single fullscreen WGSL pipeline applies:
  - display min/max
  - gamma
  - LUT selection
  - mode-specific color mapping
- When the WGPU preview path is active for intensity, red/blue, or signed-count modes, the GUI now
  requests raw preview events and can accumulate the count preview on the GPU into storage buffers
  before shading the final display texture.
- The WGPU path can also build the full histogram for those count-based modes from the GPU
  accumulation buffers, which keeps the histogram window comparison aligned with the GPU-rendered
  frame instead of forcing a second CPU-side rescan.
- `TimeSurface` now owns a persistent GPU last-tick buffer plus dedicated compute/render/histogram
  pipelines in the WGPU renderer:
  - raw preview events update the GPU tick surface with `atomicMax(last_tick, event_tick)`
  - the fullscreen render pass computes decay directly from that GPU tick surface
  - histogram and auto-contrast requests use GPU histogram compute with a sampled path for
    auto-contrast and full-bin readback for the histogram window
  - hover readout uses a tiny single-pixel GPU readback with caching instead of CPU scratch state
- The CPU fallback path still materializes the grayscale decay image locally for `glow` and for
  runtime fallback when WGPU initialization or rendering fails.
- Preview tools still read from `PreviewFrame`; the GPU count-accumulation path is additive and
  does not remove the CPU frame planes or introduce full-frame GPU readback.

## Benchmarking

Run CPU-side preview microbenchmarks with:

```bash
cargo bench -p augur-gui --bench preview_bench -- --warm-up-time 0.1 --measurement-time 0.1
```

These fixtures are deterministic synthetic frames, not live camera captures. They are useful for tracking CPU-side regressions in preview preparation and fallback rendering, but they are not a valid glow-versus-wgpu renderer comparison by themselves.

For a renderer A/B comparison, use the same replay or live workload in `--release`, keep preview mode, zoom, window size, and preview cadence fixed, hit `Reset timings`, and compare `Frame total` between `AUGUR_RENDERER=glow` and `AUGUR_RENDERER=wgpu`.

## Verification

```bash
cargo fmt --all
cargo check -p augur-core
cargo check -p augur-gui
cargo test -p augur-gui
cargo test -p augur-core
cargo clippy -p augur-gui --all-targets -- -D warnings
cargo bench -p augur-gui --bench preview_bench -- --warm-up-time 0.1 --measurement-time 0.1
```

## Notes

- `augur-core` keeps CPU frame planes and cached histograms because viewer tools, plugins, and external bridges consume `PreviewFrame` on the CPU.
- `TimeSurface` is fully GPU-backed only when the WGPU renderer is active and raw preview events are available; the CPU fallback path remains intact for `glow` and runtime fallback.
- Host-view density/image renderers use their own CPU texture path.
