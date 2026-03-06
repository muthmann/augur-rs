# ROI Grid Computation From Hotpixel Mask

## Summary

When hotpixels are masked, the remaining sensor area can be partitioned into a grid of hotpixel-free rectangular ROIs. This feature computes that grid and identifies the largest contiguous free regions, letting users pick an ROI that avoids all masked pixels.

## Algorithm

1. Build boundaries from hotpixel coordinates plus sensor edges.
2. Mark the blocked cells that contain masked pixels.
3. Enumerate free cells as candidate ROIs.
4. Run a maximal-rectangle pass to find the largest merged regions.

## Complexity

At most 64 hotpixels produce a grid of roughly 16,641 cells, so the computation stays comfortably sub-millisecond.

## Files

- `augur-core/src/analysis/roi_grid.rs`: core algorithm and unit tests
- `augur-core/src/analysis/mod.rs`: module registration and overlay wiring
- `augur-gui/src/plugins/roi_grid.rs`: plugin state, settings UI, and "Use as ROI" flow
- `augur-gui/src/preview.rs`: grid overlay rendering

## GUI Workflow

1. Enable `ROI Grid` from the toolbar `Analysis` dropdown.
2. Open the ROI Grid section in the right-side analysis panel.
3. Click `Compute ROI Grid`.
4. Toggle the overlay if you want a visual guide on the preview.
5. Use `Top N` and `Use as ROI` to pick a candidate rectangle.

## Verification

- 8 unit tests cover empty masks, corners, consecutive hotpixels, and top-K rectangle limiting
- `cargo test -p augur-core -- roi_grid`
