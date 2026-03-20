use std::cell::RefCell;

use augur_core::{
    analysis::{roi_grid::RoiGrid, Overlay},
    pipeline::PreviewFrame,
};
use egui::{Color32, ColorImage};

thread_local! {
    static PREVIEW_SCRATCH: RefCell<PreviewRenderScratch> = RefCell::new(PreviewRenderScratch::default());
}

#[derive(Default)]
struct PreviewRenderScratch {
    hist: Vec<u32>,
    grid_col_map: Vec<usize>,
    grid_row_map: Vec<usize>,
    grid_x_line: Vec<bool>,
    grid_y_line: Vec<bool>,
    grid_cell_highlight: Vec<bool>,
}

#[derive(Clone, Copy)]
struct PixelRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

pub fn frame_to_color_image(
    frame: &PreviewFrame,
    overlays: &[Overlay],
    contrast_percentile: f32,
) -> ColorImage {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let max = percentile_value_two(&frame.pixels_on, &frame.pixels_off, contrast_percentile) as f32;
    let mut image = ColorImage::new([w, h], Color32::TRANSPARENT);

    // Look for a ROI grid overlay to merge into the base rendering pass.
    let roi = overlays.iter().find_map(|o| match o {
        Overlay::RoiGrid {
            grid,
            highlight_top_n,
        } => Some((grid.as_ref(), *highlight_top_n)),
        _ => None,
    });

    if let Some((grid, top_n)) = roi {
        PREVIEW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            render_base_with_grid(
                frame,
                grid,
                top_n,
                max,
                w,
                h,
                &mut image.pixels,
                &mut scratch,
            );
        });
    } else {
        render_base(frame, max, &mut image.pixels);
    }

    render_overlays(frame, overlays, &mut image.pixels, w, h);

    image
}

#[cfg(test)]
fn percentile_value(pixels: &[u16], percentile: f32) -> u16 {
    percentile_value_from_slices(&[pixels], percentile)
}

fn percentile_value_two(on: &[u16], off: &[u16], percentile: f32) -> u16 {
    percentile_value_from_slices(&[on, off], percentile)
}

fn percentile_value_from_slices(channels: &[&[u16]], percentile: f32) -> u16 {
    PREVIEW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        percentile_value_from_slices_with_hist(channels, percentile, &mut scratch.hist)
    })
}

fn percentile_value_from_slices_with_hist(
    channels: &[&[u16]],
    percentile: f32,
    hist_scratch: &mut Vec<u32>,
) -> u16 {
    let max_val = channels
        .iter()
        .flat_map(|channel| channel.iter())
        .copied()
        .max()
        .unwrap_or(0) as usize;
    if max_val == 0 {
        return 1;
    }

    // Cap histogram at 4096 entries (16 KB). Values above the cap are clamped
    // into the last bin, so a single hot pixel at 65535 does not force a
    // 256 KB allocation every frame.
    const MAX_BINS: usize = 4096;
    let hist_size = max_val.min(MAX_BINS - 1) + 1;
    hist_scratch.resize(hist_size, 0);
    hist_scratch.fill(0);
    for channel in channels {
        for &value in *channel {
            hist_scratch[(value as usize).min(hist_size - 1)] += 1;
        }
    }

    let percentile = percentile.clamp(0.0, 100.0);
    let len = channels.iter().map(|channel| channel.len()).sum::<usize>();
    let target =
        ((len as f64 * percentile as f64 / 100.0).ceil() as u64).clamp(1, len.max(1) as u64);
    let mut cumulative = 0u64;
    for (value, &count) in hist_scratch.iter().enumerate() {
        cumulative += u64::from(count);
        if cumulative >= target {
            return value.max(1) as u16;
        }
    }

    hist_size.max(1) as u16
}

fn render_base(frame: &PreviewFrame, max: f32, pixels: &mut [Color32]) {
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let r = normalized_channel(frame.pixels_off[i], max);
        let g = normalized_channel(frame.pixels_on[i], max);
        *pixel = Color32::from_rgb(r.max(8), g.max(8), 8);
    }
}

fn normalized_channel(value: u16, max: f32) -> u8 {
    let clamped = (value as f32).min(max);
    ((clamped / max).sqrt() * 255.0) as u8
}

/// Single-pass base frame + ROI grid overlay rendering.
///
/// Merges cell tinting, grid lines, and highlight fills into one sequential
/// scan over the pixel buffer, replacing the previous multi-pass approach that
/// called `fill_rect` per grid cell, `draw_vline`/`draw_hline` per boundary,
/// and `fill_rect` again per highlight rect.
#[allow(clippy::too_many_arguments)]
fn render_base_with_grid(
    frame: &PreviewFrame,
    grid: &RoiGrid,
    top_n: usize,
    max: f32,
    w: usize,
    h: usize,
    pixels: &mut [Color32],
    scratch: &mut PreviewRenderScratch,
) {
    let grid_cols = grid.cols();
    let grid_rows = grid.rows();

    // O(W + H): map each pixel coordinate to its grid cell index.
    build_pixel_to_grid_map(&grid.x_bounds, w, &mut scratch.grid_col_map);
    build_pixel_to_grid_map(&grid.y_bounds, h, &mut scratch.grid_row_map);

    // O(W + H): boolean flags for grid line positions.
    scratch.grid_x_line.resize(w, false);
    scratch.grid_x_line.fill(false);
    for &xb in &grid.x_bounds {
        let xb = xb as usize;
        if xb > 0 && xb < w {
            scratch.grid_x_line[xb] = true;
        }
    }
    scratch.grid_y_line.resize(h, false);
    scratch.grid_y_line.fill(false);
    for &yb in &grid.y_bounds {
        let yb = yb as usize;
        if yb > 0 && yb < h {
            scratch.grid_y_line[yb] = true;
        }
    }

    // Mark grid cells inside top-N highlight rectangles.
    let n = top_n.min(grid.largest_rects.len());
    scratch
        .grid_cell_highlight
        .resize(grid_rows * grid_cols, false);
    scratch.grid_cell_highlight.fill(false);
    for rect in &grid.largest_rects[..n] {
        let c0 = grid_bound_index(&grid.x_bounds, rect.x);
        let c1 = grid_bound_index(&grid.x_bounds, rect.x + rect.width).min(grid_cols);
        let r0 = grid_bound_index(&grid.y_bounds, rect.y);
        let r1 = grid_bound_index(&grid.y_bounds, rect.y + rect.height).min(grid_rows);
        for r in r0..r1 {
            for c in c0..c1 {
                scratch.grid_cell_highlight[r * grid_cols + c] = true;
            }
        }
    }

    // Tint colors as u32 arrays for arithmetic.
    const LINE: [u32; 4] = [0, 220, 220, 100];
    const HIGHLIGHT: [u32; 4] = [80, 255, 120, 72];
    const FREE: [u32; 4] = [40, 220, 80, 36];
    const BLOCKED: [u32; 4] = [255, 40, 40, 80];

    // Single sequential pass - perfect cache locality, no per-cell function calls.
    for py in 0..h {
        let gr = scratch.grid_row_map[py];
        let on_hline = scratch.grid_y_line[py];
        let row_off = py * w;

        for px in 0..w {
            let on_v = frame.pixels_on[row_off + px];
            let off_v = frame.pixels_off[row_off + px];
            let base_g = u32::from(normalized_channel(on_v, max).max(8));
            let base_r = u32::from(normalized_channel(off_v, max).max(8));

            let gc = scratch.grid_col_map[px];
            let cell = gr * grid_cols + gc;

            let tint = if on_hline || scratch.grid_x_line[px] {
                LINE
            } else if scratch.grid_cell_highlight[cell] {
                HIGHLIGHT
            } else if grid.blocked[cell] {
                BLOCKED
            } else {
                FREE
            };

            let a = tint[3];
            let inv = 255 - a;
            let r = ((base_r * inv + tint[0] * a) / 255) as u8;
            let g = ((base_g * inv + tint[1] * a) / 255) as u8;
            let b = ((8 * inv + tint[2] * a) / 255) as u8;
            pixels[row_off + px] = Color32::from_rgb(r, g, b);
        }
    }

    // Yellow borders for highlight rects - thin lines, negligible cost.
    let border_color = [255u8, 220, 0, 200];
    for rect in &grid.largest_rects[..n] {
        draw_rect_border(
            pixels,
            [w, h],
            PixelRect {
                x: rect.x as usize,
                y: rect.y as usize,
                width: rect.width as usize,
                height: rect.height as usize,
            },
            2,
            border_color,
        );
    }
}

/// Linear sweep mapping each pixel coordinate to its grid cell index.
fn build_pixel_to_grid_map(bounds: &[u16], size: usize, out: &mut Vec<usize>) {
    out.resize(size, 0);
    let max_idx = bounds.len().saturating_sub(2);
    let mut gi = 0;
    for (px, slot) in out.iter_mut().enumerate() {
        while gi < max_idx && (px as u16) >= bounds[gi + 1] {
            gi += 1;
        }
        *slot = gi;
    }
}

/// Find the boundary index for a grid-aligned value.
fn grid_bound_index(bounds: &[u16], val: u16) -> usize {
    bounds.partition_point(|&b| b < val)
}

fn blend_color(dst: &mut Color32, src: [u8; 4]) {
    let [dr, dg, db, _da] = dst.to_array();
    let alpha = u32::from(src[3]);
    let inv_alpha = 255 - alpha;

    let r = ((u32::from(dr) * inv_alpha + u32::from(src[0]) * alpha) / 255) as u8;
    let g = ((u32::from(dg) * inv_alpha + u32::from(src[1]) * alpha) / 255) as u8;
    let b = ((u32::from(db) * inv_alpha + u32::from(src[2]) * alpha) / 255) as u8;
    *dst = Color32::from_rgb(r, g, b);
}

fn draw_crosshair(
    pixels: &mut [Color32],
    size: [usize; 2],
    x: f32,
    y: f32,
    arm_len: usize,
    color: [u8; 4],
) {
    let [width, height] = size;
    let cx = x.round() as isize;
    let cy = y.round() as isize;
    let arm_len = arm_len.max(2) as isize;

    for dx in -arm_len..=arm_len {
        if dx.abs() <= 1 {
            continue;
        }
        blend_pixel(pixels, width, height, cx + dx, cy, color);
    }
    for dy in -arm_len..=arm_len {
        if dy.abs() <= 1 {
            continue;
        }
        blend_pixel(pixels, width, height, cx, cy + dy, color);
    }
    blend_pixel(pixels, width, height, cx, cy, color);
}

fn blend_pixel(
    pixels: &mut [Color32],
    width: usize,
    height: usize,
    x: isize,
    y: isize,
    color: [u8; 4],
) {
    if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
        return;
    }
    let idx = y as usize * width + x as usize;
    blend_color(&mut pixels[idx], color);
}

fn draw_rect_border(
    pixels: &mut [Color32],
    size: [usize; 2],
    rect: PixelRect,
    thickness: usize,
    color: [u8; 4],
) {
    let [width, height] = size;
    let x_end = (rect.x + rect.width).min(width);
    let y_end = (rect.y + rect.height).min(height);

    // Top and bottom edges.
    for t in 0..thickness {
        if rect.y + t < y_end {
            for px in rect.x..x_end {
                let idx = (rect.y + t) * width + px;
                blend_color(&mut pixels[idx], color);
            }
        }
        if y_end > t && y_end - 1 - t >= rect.y {
            let by = y_end - 1 - t;
            for px in rect.x..x_end {
                let idx = by * width + px;
                blend_color(&mut pixels[idx], color);
            }
        }
    }
    // Left and right edges - skip rows already covered by the top/bottom passes.
    let inner_y = (rect.y + thickness).min(y_end);
    let inner_y_end = y_end.saturating_sub(thickness).max(inner_y);
    for py in inner_y..inner_y_end {
        for t in 0..thickness {
            if rect.x + t < x_end {
                let idx = py * width + rect.x + t;
                blend_color(&mut pixels[idx], color);
            }
            if x_end > t && x_end - 1 - t >= rect.x {
                let rx = x_end - 1 - t;
                let idx = py * width + rx;
                blend_color(&mut pixels[idx], color);
            }
        }
    }
}

fn render_overlays(
    frame: &PreviewFrame,
    overlays: &[Overlay],
    pixels: &mut [Color32],
    w: usize,
    h: usize,
) {
    // Sparse overlays (HighlightPixels) as a separate pass - only touches marked pixels.
    for overlay in overlays {
        match overlay {
            Overlay::HighlightPixels {
                pixels: highlighted,
                color,
            } => {
                for pixel in highlighted {
                    if pixel.x >= frame.width || pixel.y >= frame.height {
                        continue;
                    }
                    let idx = pixel.y as usize * w + pixel.x as usize;
                    blend_color(&mut pixels[idx], *color);
                }
            }
            Overlay::CrosshairMarkers {
                markers,
                color,
                arm_len,
            } => {
                for marker in markers {
                    draw_crosshair(
                        pixels,
                        [w, h],
                        marker.x,
                        marker.y,
                        usize::from(*arm_len),
                        *color,
                    );
                }
            }
            Overlay::RoiGrid { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{frame_to_color_image, percentile_value, percentile_value_two};
    use augur_core::{
        analysis::{roi_grid::compute_roi_grid, Overlay, Pixel, SubpixelMarker},
        pipeline::PreviewFrame,
    };
    use std::sync::Arc;

    #[test]
    fn percentile_ignores_single_hotpixel_outlier() {
        let pixels = [1, 2, 2, 3, 10_000];
        assert_eq!(percentile_value(&pixels, 80.0), 3);
    }

    #[test]
    fn percentile_returns_one_for_empty_or_all_zero() {
        assert_eq!(percentile_value(&[], 99.5), 1);
        assert_eq!(percentile_value(&[0, 0, 0], 99.5), 1);
    }

    #[test]
    fn combined_percentile_uses_both_polarity_channels() {
        let on = [0, 3];
        let off = [0, 6];
        assert_eq!(percentile_value_two(&on, &off, 75.0), 3);
    }

    #[test]
    fn frame_to_color_image_renders_sparse_overlays_with_direct_color32_pixels() {
        let frame = PreviewFrame {
            width: 2,
            height: 1,
            pixels: vec![10, 20],
            pixels_on: vec![5, 25],
            pixels_off: vec![7, 30],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };
        let overlays = vec![
            Overlay::HighlightPixels {
                pixels: vec![Pixel::new(1, 0)],
                color: [255, 0, 0, 255],
            },
            Overlay::CrosshairMarkers {
                markers: vec![SubpixelMarker { x: 0.0, y: 0.0 }],
                color: [0, 255, 0, 255],
                arm_len: 2,
            },
        ];

        let image = frame_to_color_image(&frame, &overlays, 80.0);

        assert_eq!(image.size, [2, 1]);
        assert_eq!(image.pixels.len(), 2);
        assert!(image.pixels[1].to_array()[0] >= image.pixels[0].to_array()[0]);
        assert_eq!(image.pixels[0].to_array()[1], 255);
    }

    #[test]
    fn roi_grid_overlay_renders_without_allocating_rgba_intermediates() {
        let grid = compute_roi_grid(&[(0, 0)], 2, 2, 1);
        let frame = PreviewFrame {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 4],
            pixels_on: vec![1, 2, 3, 4],
            pixels_off: vec![1, 2, 3, 4],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };
        let overlays = vec![Overlay::RoiGrid {
            grid: Arc::new(grid),
            highlight_top_n: 1,
        }];

        let image = frame_to_color_image(&frame, &overlays, 50.0);

        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels.len(), 4);
    }
}
