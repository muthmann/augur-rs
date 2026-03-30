# ROI Grid Computation from Hotpixel Mask

## Summary

When hotpixels are masked, the remaining sensor area can be partitioned into a grid of hotpixel-free rectangular ROIs. This feature computes that grid and identifies the largest contiguous free regions, letting users pick an optimal ROI that avoids all masked pixels.

## Algorithm

1. **Build boundaries**: Collect all unique x/y coordinates from hotpixels (plus sensor edges `0` and the current configured sensor width/height). Each hotpixel at `(hx, hy)` inserts `hx`, `hx+1` (and `hy`, `hy+1`) to isolate it in a 1-pixel-wide cell.
2. **Mark blocked cells**: Each hotpixel maps to exactly one grid cell via binary search.
3. **Enumerate free cells**: Every unblocked cell is a valid ROI candidate with known `(x, y, w, h)`.
4. **Maximal rectangle**: Stack-based maximal-rectangle-in-histogram algorithm (weighted by actual pixel dimensions) finds the top-K largest merged free regions.

### Complexity

At most 64 hotpixels (IMX636 DEM limit) produce a grid of at most ~129x129 = 16,641 cells. All operations are sub-millisecond.

## Key Types

- `SensorRect { x, y, width, height }` — rectangle in sensor pixel coordinates
- `RoiGrid { x_bounds, y_bounds, blocked, free_cells, largest_rects, sensor_width, sensor_height }`
- `Overlay::RoiGrid { grid, highlight_top_n }` — overlay variant for rendering

## Files

| File | Role |
|------|------|
| `augur-core/src/analysis/roi_grid.rs` | Core algorithm + unit tests |
| `augur-core/src/analysis/mod.rs` | Module registration, `Overlay::RoiGrid` variant |
| `augur-gui/src/plugins/roi_grid.rs` | GUI plugin state, ROI-grid settings UI, recompute logic, "Use as ROI" wiring |
| `augur-gui/src/preview.rs` | Grid overlay rendering (grid lines, cell tints, highlighted rects) |
| `augur-gui/src/plugin.rs` | Built-in plugin trait used by ROI Grid |

## GUI Workflow

1. Enable **ROI Grid** from the toolbar **Analysis** dropdown.
2. Open the ROI Grid section in the right-side **Analysis Tools** panel.
3. Click **Compute ROI Grid** — grid is computed and overlay shown.
4. Toggle **Show ROI Grid overlay** to visualize grid lines and regions on the preview.
5. Adjust **Top N** to control how many largest rectangles are highlighted.
6. Click **Use as ROI** next to any listed rectangle to copy it into the ROI config.

Auto-recomputes when `masked_pixels` changes after the grid has been computed once, even if the overlay is temporarily hidden.
The computation uses the current sensor geometry from the top-bar `Settings` menu instead of a hardcoded IMX636 size.

## Overlay Rendering

- **Grid lines**: semi-transparent cyan at each boundary
- **Free cells**: subtle green tint
- **Blocked cells**: red tint
- **Top-N largest rects**: yellow 2px borders + stronger green fill

## Edge Cases

- No hotpixels: single cell = full frame
- Hotpixel at `(0,0)` or `(sensor_width - 1, sensor_height - 1)`: boundary cells handled correctly
- Consecutive hotpixels (e.g. x=100, x=101): no spurious gap cell

## Verification

- 8 unit tests covering: no hotpixels, single hotpixel, corners, consecutive, full 64, top-K limiting, hotpixel avoidance
- `cargo test -p augur-core -- roi_grid`
