# WGPU Preview Rendering

## Summary

This feature implements phases 1-4 of the preview-rendering plan for `augur-gui`.

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

## Benchmark Baseline

Benchmark command run on 2026-04-03:

```bash
cargo bench -p augur-gui --bench preview_bench -- --warm-up-time 0.1 --measurement-time 0.1
```

Reference machine:

- model: `MacBookPro18,3`
- CPU: `Apple M1 Pro`
- arch: `arm64`
- macOS: `15.6`

These fixtures are deterministic synthetic replay-style frames, not live camera captures.
They are CPU microbenchmarks only. They are useful for tracking CPU-side regressions in preview
preparation and fallback rendering, but they are not a valid glow-versus-wgpu renderer A/B by
themselves.

| Benchmark | Fixture | Reported time |
| --- | --- | --- |
| `cpu_preview/frame_to_color_image` | `320x240 sparse` | `[265.35 µs, 323.44 µs, 392.93 µs]` |
| `cpu_preview/frame_to_color_image` | `320x240 medium` | `[155.05 µs, 158.11 µs, 161.73 µs]` |
| `cpu_preview/frame_to_color_image` | `320x240 dense` | `[167.39 µs, 174.13 µs, 183.87 µs]` |
| `cpu_preview/frame_to_color_image` | `1280x720 sparse` | `[1.2753 ms, 1.3095 ms, 1.3469 ms]` |
| `cpu_preview/frame_to_color_image` | `1280x720 medium` | `[1.3315 ms, 1.3778 ms, 1.4290 ms]` |
| `cpu_preview/frame_to_color_image` | `1280x720 dense` | `[1.7978 ms, 2.1800 ms, 2.7714 ms]` |
| `cpu_preview/histogram` | `1280x720 sparse` | `[776.91 µs, 799.00 µs, 826.06 µs]` |
| `cpu_preview/histogram` | `1280x720 medium` | `[768.15 µs, 776.93 µs, 786.57 µs]` |
| `cpu_preview/histogram` | `1280x720 dense` | `[820.61 µs, 920.67 µs, 1.0519 ms]` |
| `cpu_preview/time_surface_prepare` | `1280x720 sparse` | `[2.7885 ms, 2.8487 ms, 2.9113 ms]` |
| `cpu_preview/time_surface_prepare` | `1280x720 medium` | `[3.1061 ms, 3.3224 ms, 3.5667 ms]` |
| `cpu_preview/time_surface_prepare` | `1280x720 dense` | `[5.8335 ms, 5.8976 ms, 5.9650 ms]` |
| `cpu_preview/line_profile` | `1280x720 sparse` | `[5.2427 µs, 5.3982 µs, 5.5823 µs]` |
| `cpu_preview/line_profile` | `1280x720 medium` | `[5.3689 µs, 5.5120 µs, 5.6926 µs]` |
| `cpu_preview/line_profile` | `1280x720 dense` | `[5.2363 µs, 5.3866 µs, 5.5750 µs]` |

The main takeaway from this baseline is that the hottest CPU-side steps before GPU accumulation are
still full-frame synthesis, histogram work, and especially time-surface preparation on the CPU
fallback path. The WGPU path now bypasses the old CPU-owned time-surface tick/decay preparation for
normal preview rendering, histogram work, and hover-value queries, while count-based preview modes
can additionally shift count accumulation plus full histogram generation onto the GPU when raw
preview events are available. CPU-side time-surface preparation still exists for compatibility-mode
and fallback behavior.

For renderer A/B, use the same replay or live workload in `--release`, keep preview mode, zoom,
window size, and preview cadence fixed, hit `Reset timings`, and compare `Frame total` between
`AUGUR_RENDERER=glow` and `AUGUR_RENDERER=wgpu`.

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

## Follow-Up

- Upstream `augur-core` still keeps CPU frame planes and cached histograms because viewer tools,
  plugins, and external bridges consume `PreviewFrame` on the CPU.
- `TimeSurface` is fully GPU-backed only when the WGPU renderer is active and raw preview events
  are available; the CPU fallback path remains intentionally intact for `glow`, compatibility, and
  runtime fallback.
- Host-view density/image renderers still use their existing CPU texture path.
- Manual smoke coverage is still worthwhile on:
  - live Apple Silicon preview with `AUGUR_RENDERER=auto`
  - forced `AUGUR_RENDERER=glow`
  - replay popup preview on `wgpu`
