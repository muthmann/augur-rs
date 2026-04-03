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
- Keep CPU accumulation for phases 1-4, but move preview presentation math to the GPU when the
  `wgpu` path is active.
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
- The app preserves compatibility because `glow` remains available and startup can fall back
  automatically.
- Main-window and popup preview presentation now share one renderer contract instead of drifting
  apart.

### Negative

- The preview system is more complex because it now owns two rendering paths plus runtime fallback
  behavior.
- Texture/resource lifetime management becomes part of the GUI code.
- Shader-side presentation is only a partial GPU migration; time-surface decay and event
  accumulation are still CPU-side in this pass.
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

Deferred. Compute accumulation has a higher ceiling, especially on unified-memory Apple Silicon,
but it also carries more design and portability risk. Phases 1-4 intentionally stop at
CPU-accumulate plus GPU-present.
