# ADR 035 — Preview compute dispatches are sized against the device's limit

- **Status:** Accepted
- **Date:** 2026-08-05
- **Relates to:** ADR 034 (crash visibility — this is the first crash it caught),
  [WGPU Preview Rendering](../features/wgpu-preview-rendering.md)

## Context

An unattended A1 protocol survey killed Augur on row 14 of 54:

```
PANIC  wgpu error: Validation Error
    In a ComputePass
      note: encoder = `augur_count_accumulate_encoder`
    In a dispatch command, indirect:false
      note: compute pipeline = `augur_count_accumulate_pipeline`
    Each current dispatch group size dimension ([77375, 1, 1]) must be less or
    equal to 65535
  while: recording …_p14_u300m_f2Hz_a1700m (protocol row 14/54)
```

Every compute pass in the preview renderer ran one invocation per item — one
per event, one per pixel — as a flat `(n, 1, 1)` dispatch:

```rust
fn dispatch_workgroups(items: u32) -> u32 {
    items.max(1).div_ceil(COUNT_WORKGROUP_SIZE)
}
```

That is the natural shape and it is correct up to a point. A dispatch dimension
is capped at `max_compute_workgroups_per_dimension` — 65535 in wgpu's guaranteed
floor and on every desktop backend — so at the 64-wide workgroup this file uses,
the flat form dies above **4 194 240 items**.

The row that hit it was 2 Hz at `a = 1.7`: a low frequency and a high contrast,
which is the corner of the survey that produces the most events per preview
frame. It delivered 4 952 000 in one frame, asking for 77 375 workgroups.

Two things made this worse than an ordinary bug:

- **It is a panic, not an error.** wgpu validation failures panic, so this took
  the process down rather than dropping a frame. The recording, the drive lease
  and the remaining 40 rows went with it.
- **The limit was never stated anywhere.** The helper's name says how many
  workgroups you need, not how many you are allowed. Nothing failed until the
  bench reached a point bright enough and slow enough to cross it, which took
  months of use to happen.

## Decision

**Lay out a dispatch as a grid that cannot exceed the device's limit, and let
the shader recover the linear index.**

`DispatchGrid::for_items(items, max_per_dimension)` fills `x` up to the limit
and then grows into `y`. The shader reads

```wgsl
let index = gid.x + gid.y * uniforms.dispatch_width;
```

where `dispatch_width` is one grid row's worth of invocations (`x × 64`). For a
single-row dispatch `gid.y` is 0 and this is exactly the old behaviour, so the
ordinary case is unchanged rather than merely equivalent.

**The limit is read from the live device**, not hard-coded. It is a property of
the adapter; a machine that allows more should not be forced into a taller grid,
and — more to the point — the number that matters is the real one.

**All four dispatch sites use it**, not only the two that could realistically
overflow. The two event-driven passes (count accumulate, time-surface
accumulate) are the ones that crashed. The two histogram passes are bounded by
sensor resolution and cannot reach the limit on any current sensor — but "cannot
reach it on current hardware" is an unstated assumption of exactly the kind that
caused this, so they are given the same treatment.

**Nothing is dropped.** Capping the item count instead would have been a smaller
change, and would have made the preview silently wrong on the brightest points
in the survey — the ones the survey is about. Losing a frame is acceptable;
under-counting one without saying so is not.

## Consequences

- The preview renders correctly at any event count a `u32` can express, rather
  than crashing above 4.19 M per frame.
- `CountPreviewUniforms` grows from 32 to 48 bytes (one field plus padding to
  the 16-byte uniform alignment). The two time-surface uniform structs absorbed
  the field into existing padding and are unchanged in size.
- The linearization is a silent-corruption risk if it and the grid ever
  disagree, so it is tested by *executing* the real shader over a forced 2D grid
  and checking the counts — not by argument. The test was confirmed to fail
  against a deliberately reverted shader (256 counts instead of 200, each grid
  row re-reading the same events).
- The shader sources are also validated against a real device in a test, because
  WGSL is compiled by the driver at pipeline creation: a mistake in them is a
  runtime fault on the bench, not a build error.
- This was found only because the session log from ADR 034 recorded the panic
  and the row it happened on. Without it the same run would have been a second
  silent disappearance.
