# ADR 015: Dual-Backend Preview Rendering With WGPU Presentation

## Status

Accepted

## Context

The existing preview path in `augur-gui` was still dominated by CPU-side frame synthesis and full
RGBA texture uploads on the egui thread. That left three issues:

- preview rendering and texture upload remained a visible bottleneck
- display-only changes such as min/max/gamma/LUT still paid a full CPU colorization cost
- the app had no Metal/D3D12/Vulkan-backed preview path even though Apple Silicon is a primary
  target

At the same time, Augur still needs broad compatibility:

- older or problematic systems must keep an OpenGL path
- preview failures must never threaten capture-to-disk behavior
- viewer tools must keep reading directly from `PreviewFrame` without GPU readback or CPU/GPU
  ping-pong

## Decision

Adopt a dual-backend preview architecture inside `augur-gui`.

- Keep `eframe` as the app shell and enable both `wgpu` and `glow`.
- Add runtime renderer selection through `AUGUR_RENDERER=glow|wgpu|auto`.
- Define `auto` as: try `wgpu`, retry `glow` automatically on startup failure.
- Keep the preview contract centered on `PreviewFrame`; do not change plugin ABI or replay formats.
- Split preview work into:
  - frame-derived payload preparation
  - derived viewer work such as histogram and line profile
  - renderer submission
- Keep the `PreviewFrame` CPU contract intact, but allow the WGPU path to do count-based preview
  accumulation and histogram generation on the GPU when raw preview events are available.
- Let the WGPU `TimeSurface` path own its authoritative last-event-tick surface on the GPU and use
  that state for accumulation, decay rendering, histogram generation, and hover queries.
- Keep the incremental CPU histogram fallback in `augur-core` so `glow`, tools, plugins, and
  compatibility paths still avoid full-frame rescans.
- Use one shared `PreviewRenderer` abstraction for the main viewer and popup viewer.
- Keep viewer tools on CPU and forbid GPU readback in the normal preview path.
- Move analysis overlays out of the preview image and paint them with the viewer painter on top of
  the displayed texture.

## Consequences

### Positive

- Apple Silicon now gets a Metal-backed preview path through `wgpu`.
- Windows and Linux gain D3D12/Vulkan-capable preview presentation without rewriting the GUI.
- Intensity, red/blue, and signed-count preview modes can apply display math in shader code instead
  of CPU colorization.
- Intensity, red/blue, and signed-count preview modes can also accumulate their preview counts and
  build the full histogram on the GPU in the WGPU path when raw preview events are present.
- The WGPU `TimeSurface` path can keep its last-event-tick surface on the GPU, update it directly
  from raw preview events, render decay without a CPU-owned tick upload, and read back only small
  histogram / hover-query buffers when CPU-side UI state needs them.
- The CPU fallback and tool/plugin paths still benefit from incremental upstream histogram caches
  instead of full-frame rescans.
- The app preserves compatibility because `glow` remains available and startup can fall back
  automatically.
- Main-window and popup preview presentation now share one renderer contract instead of drifting
  apart.

### Negative

- The preview system is more complex because it now owns two rendering paths plus runtime fallback
  behavior.
- Texture/resource lifetime management becomes part of the GUI code.
- The preview stack is now hybrid: CPU frame planes remain authoritative for tools/plugins, while
  the WGPU preview path can perform extra GPU-side accumulation for count-based presentation and
  histogram display, and can maintain its own authoritative GPU time-surface state.
- Requesting raw preview events for WGPU count modes increases preview-side event-copy pressure, so
  the new GPU accumulation path must continue to be latest-only and must never block capture.
- WGPU `TimeSurface` now also depends on raw preview events and persistent GPU state reset rules,
  so geometry changes and timestamp regressions must clear GPU buffers just like the old CPU cache
  did.
- Very old or driver-broken systems may still need manual fallback to `AUGUR_RENDERER=glow`.

## Alternatives Considered

### Keep the CPU-colored `ColorImage` path only

Rejected because it leaves CPU-side colorization and upload costs in the hottest preview loop and
does not take advantage of Metal/D3D12/Vulkan-class hardware.

### Rewrite the preview as a separate `winit + wgpu` application surface

Rejected for this pass because it would raise migration risk and duplicate too much existing
`eframe`/egui integration. The current goal is to raise the preview ceiling while preserving the
existing GUI structure.

### Move event accumulation to the GPU immediately

Partially adopted. This ADR now allows targeted GPU accumulation for count-based preview and
histogram work plus a fully GPU-owned WGPU `TimeSurface` path, but `augur-core` still keeps CPU
frame planes for tools, plugins, and bridges. A future pass may revisit whether some of that CPU
duplication can be reduced safely.
