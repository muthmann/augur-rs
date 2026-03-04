//! ROI grid computation from a hotpixel mask.
//!
//! Each hotpixel defines grid lines that split the sensor into strips. By
//! collecting all unique x/y coordinates from hotpixels (plus sensor edges),
//! we get a grid where each cell is guaranteed to either fully contain or
//! fully avoid every hotpixel. The maximal-rectangle-in-histogram algorithm
//! then finds the largest contiguous hotpixel-free regions.

/// A rectangle in sensor pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorRect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl SensorRect {
    pub fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }
}

/// Result of partitioning the sensor into a hotpixel-aware grid.
#[derive(Debug, Clone, PartialEq)]
pub struct RoiGrid {
    /// Sorted x-boundary positions (including 0 and sensor_width).
    pub x_bounds: Vec<u16>,
    /// Sorted y-boundary positions (including 0 and sensor_height).
    pub y_bounds: Vec<u16>,
    /// Row-major grid: `blocked[row][col]` is true if the cell contains a hotpixel.
    pub blocked: Vec<Vec<bool>>,
    /// All free (unblocked) cells as sensor rectangles.
    pub free_cells: Vec<SensorRect>,
    /// Top-K largest merged rectangles (sorted by area, largest first).
    pub largest_rects: Vec<SensorRect>,
    pub sensor_width: u16,
    pub sensor_height: u16,
}

/// Compute the ROI grid from a list of hotpixel coordinates.
///
/// Returns a [`RoiGrid`] with grid boundaries, blocked/free cells, and the
/// `top_k` largest hotpixel-free merged rectangles (found via the maximal
/// rectangle in histogram algorithm, weighted by actual pixel dimensions).
pub fn compute_roi_grid(
    hotpixels: &[(u16, u16)],
    sensor_width: u16,
    sensor_height: u16,
    top_k: usize,
) -> RoiGrid {
    let x_bounds = build_bounds(hotpixels.iter().map(|&(x, _)| x), sensor_width);
    let y_bounds = build_bounds(hotpixels.iter().map(|&(_, y)| y), sensor_height);

    let cols = x_bounds.len() - 1;
    let rows = y_bounds.len() - 1;

    let mut blocked = vec![vec![false; cols]; rows];

    // Mark blocked cells.  Each hotpixel at (hx, hy) falls into the cell
    // whose x-interval contains hx and y-interval contains hy.
    for &(hx, hy) in hotpixels {
        if hx >= sensor_width || hy >= sensor_height {
            continue;
        }
        let col = find_cell(&x_bounds, hx);
        let row = find_cell(&y_bounds, hy);
        if col < cols && row < rows {
            blocked[row][col] = true;
        }
    }

    // Enumerate free cells.
    let mut free_cells = Vec::new();
    for (r, row) in blocked.iter().enumerate().take(rows) {
        for (c, &is_blocked) in row.iter().enumerate().take(cols) {
            if !is_blocked {
                free_cells.push(cell_rect(&x_bounds, &y_bounds, r, c));
            }
        }
    }

    // Find largest merged rectangles using maximal-rectangle-in-histogram.
    let largest_rects = find_largest_rects(&blocked, &x_bounds, &y_bounds, top_k);

    RoiGrid {
        x_bounds,
        y_bounds,
        blocked,
        free_cells,
        largest_rects,
        sensor_width,
        sensor_height,
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build sorted, deduplicated boundary list from hotpixel coordinates along
/// one axis.  For each coordinate `c`, inserts both `c` and `c+1` (to isolate
/// the single-pixel cell containing the hotpixel).  The sensor edge values 0
/// and `max` are always included.
fn build_bounds(coords: impl Iterator<Item = u16>, max: u16) -> Vec<u16> {
    let mut set = std::collections::BTreeSet::new();
    set.insert(0);
    set.insert(max);
    for c in coords {
        if c < max {
            set.insert(c);
            set.insert(c + 1);
        }
    }
    set.into_iter().collect()
}

/// Binary-search for the cell index that contains `val`.  The cell at index
/// `i` spans `[bounds[i], bounds[i+1])`.
fn find_cell(bounds: &[u16], val: u16) -> usize {
    match bounds.binary_search(&val) {
        Ok(i) => i.min(bounds.len().saturating_sub(2)),
        Err(i) => i.saturating_sub(1),
    }
}

/// Convert a grid cell (row, col) into a [`SensorRect`].
fn cell_rect(x_bounds: &[u16], y_bounds: &[u16], row: usize, col: usize) -> SensorRect {
    SensorRect {
        x: x_bounds[col],
        y: y_bounds[row],
        width: x_bounds[col + 1] - x_bounds[col],
        height: y_bounds[row + 1] - y_bounds[row],
    }
}

/// Maximal-rectangle-in-histogram, collecting top-K by pixel area.
///
/// We sweep row by row, maintaining a height histogram (in grid-cell units)
/// for each column.  For each row we run the standard stack-based largest-
/// rectangle algorithm, but weight each candidate rectangle by the actual
/// pixel dimensions of the cells it spans.
fn find_largest_rects(
    blocked: &[Vec<bool>],
    x_bounds: &[u16],
    y_bounds: &[u16],
    top_k: usize,
) -> Vec<SensorRect> {
    let rows = blocked.len();
    if rows == 0 {
        return Vec::new();
    }
    let cols = blocked[0].len();

    // height[c] = number of consecutive free rows ending at current row for column c
    let mut height = vec![0_usize; cols];

    // Collect all candidate rectangles across all rows, then take top-K.
    let mut candidates: Vec<SensorRect> = Vec::new();

    for (r, row) in blocked.iter().enumerate().take(rows) {
        // Update histogram.
        for (c, column_height) in height.iter_mut().enumerate().take(cols) {
            if row[c] {
                *column_height = 0;
            } else {
                *column_height += 1;
            }
        }

        // Stack-based largest rectangle in histogram.
        let mut stack: Vec<usize> = Vec::new(); // stack of column indices
        let mut c = 0;
        while c <= cols {
            let h = if c < cols { height[c] } else { 0 };
            if stack.is_empty() || h >= height[*stack.last().unwrap()] {
                stack.push(c);
                c += 1;
            } else {
                let top = stack.pop().unwrap();
                let left = stack.last().map_or(0, |&s| s + 1);
                let right = c; // exclusive

                // The rectangle spans columns [left..right) and
                // height[top] rows ending at row r.
                let grid_h = height[top];
                let row_start = r + 1 - grid_h;
                let row_end = r + 1; // exclusive

                let px_x = x_bounds[left];
                let px_w = x_bounds[right] - x_bounds[left];
                let px_y = y_bounds[row_start];
                let px_h = y_bounds[row_end] - y_bounds[row_start];

                let rect = SensorRect {
                    x: px_x,
                    y: px_y,
                    width: px_w,
                    height: px_h,
                };

                candidates.push(rect);
            }
        }
    }

    // Sort by area descending and deduplicate.
    candidates.sort_by_key(|rect| std::cmp::Reverse(rect.area()));
    candidates.dedup();
    candidates.truncate(top_k);
    candidates
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_hotpixels_yields_full_frame() {
        let grid = compute_roi_grid(&[], 1280, 720, 5);
        assert_eq!(grid.x_bounds, vec![0, 1280]);
        assert_eq!(grid.y_bounds, vec![0, 720]);
        assert_eq!(grid.free_cells.len(), 1);
        assert_eq!(grid.free_cells[0].area(), 1280 * 720);
        assert_eq!(grid.largest_rects.len(), 1);
        assert_eq!(grid.largest_rects[0].area(), 1280 * 720);
    }

    #[test]
    fn single_hotpixel_center() {
        let grid = compute_roi_grid(&[(640, 360)], 1280, 720, 5);
        // x_bounds: [0, 640, 641, 1280]  -> 3 columns
        // y_bounds: [0, 360, 361, 720]   -> 3 rows
        assert_eq!(grid.x_bounds, vec![0, 640, 641, 1280]);
        assert_eq!(grid.y_bounds, vec![0, 360, 361, 720]);

        // 9 cells total, 1 blocked, 8 free
        let total_cells: usize = grid.blocked.iter().map(|row| row.len()).sum();
        assert_eq!(total_cells, 9);
        assert_eq!(grid.free_cells.len(), 8);

        // The blocked cell is at (640, 360) with size 1x1
        assert!(grid.blocked[1][1]);
    }

    #[test]
    fn hotpixel_at_origin() {
        let grid = compute_roi_grid(&[(0, 0)], 1280, 720, 3);
        // x_bounds: [0, 1, 1280], y_bounds: [0, 1, 720]
        assert_eq!(grid.x_bounds, vec![0, 1, 1280]);
        assert_eq!(grid.y_bounds, vec![0, 1, 720]);
        assert!(grid.blocked[0][0]);
        assert_eq!(grid.free_cells.len(), 3);
    }

    #[test]
    fn hotpixel_at_corner() {
        let grid = compute_roi_grid(&[(1279, 719)], 1280, 720, 3);
        assert_eq!(*grid.x_bounds.last().unwrap(), 1280);
        assert_eq!(*grid.y_bounds.last().unwrap(), 720);
        // The bottom-right cell is blocked
        let last_row = grid.blocked.len() - 1;
        let last_col = grid.blocked[0].len() - 1;
        assert!(grid.blocked[last_row][last_col]);
        assert_eq!(grid.free_cells.len(), 3);
    }

    #[test]
    fn consecutive_hotpixels_no_gap() {
        let grid = compute_roi_grid(&[(100, 200), (101, 200)], 1280, 720, 3);
        // x_bounds should include 100, 101, 102 => [0, 100, 101, 102, 1280]
        assert!(grid.x_bounds.contains(&100));
        assert!(grid.x_bounds.contains(&101));
        assert!(grid.x_bounds.contains(&102));
        // Both hotpixel cells should be blocked
        let col_100 = find_cell(&grid.x_bounds, 100);
        let col_101 = find_cell(&grid.x_bounds, 101);
        let row = find_cell(&grid.y_bounds, 200);
        assert!(grid.blocked[row][col_100]);
        assert!(grid.blocked[row][col_101]);
    }

    #[test]
    fn largest_rect_avoids_hotpixels() {
        // A single hotpixel near the top-left: largest free rect should not contain it.
        let grid = compute_roi_grid(&[(10, 10)], 1280, 720, 3);
        for rect in &grid.largest_rects {
            // The hotpixel at (10,10) should not be inside any reported rect.
            let contains_hp = rect.x <= 10
                && 10 < rect.x + rect.width
                && rect.y <= 10
                && 10 < rect.y + rect.height;
            assert!(!contains_hp, "largest rect must not contain hotpixel");
        }
    }

    #[test]
    fn max_hotpixels_still_runs() {
        // 64 hotpixels along the diagonal
        let hps: Vec<(u16, u16)> = (0..64).map(|i| (i * 20, i * 11)).collect();
        let grid = compute_roi_grid(&hps, 1280, 720, 5);
        assert!(!grid.largest_rects.is_empty());
        // All largest rects should have non-zero area
        for r in &grid.largest_rects {
            assert!(r.area() > 0);
        }
    }

    #[test]
    fn top_k_limits_output() {
        let grid = compute_roi_grid(&[(100, 100), (500, 500)], 1280, 720, 2);
        assert!(grid.largest_rects.len() <= 2);
    }
}
