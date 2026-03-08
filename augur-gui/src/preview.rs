use augur_core::{
    analysis::{roi_grid::RoiGrid, Overlay},
    pipeline::PreviewFrame,
};
use egui::ColorImage;

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
    let mut rgba = vec![0u8; w * h * 4];

    // Look for a ROI grid overlay to merge into the base rendering pass.
    let roi = overlays.iter().find_map(|o| match o {
        Overlay::RoiGrid {
            grid,
            highlight_top_n,
        } => Some((grid.as_ref(), *highlight_top_n)),
        _ => None,
    });

    if let Some((grid, top_n)) = roi {
        render_base_with_grid(frame, grid, top_n, max, w, h, &mut rgba);
    } else {
        for i in 0..(w * h) {
            let r = normalized_channel(frame.pixels_off[i], max);
            let g = normalized_channel(frame.pixels_on[i], max);
            let off = i * 4;
            rgba[off] = r.max(8);
            rgba[off + 1] = g.max(8);
            rgba[off + 2] = 8;
            rgba[off + 3] = 255;
        }
    }

    // Sparse overlays (HighlightPixels) as a separate pass — only touches marked pixels.
    for overlay in overlays {
        match overlay {
            Overlay::HighlightPixels { pixels, color } => {
                for pixel in pixels {
                    if pixel.x >= frame.width || pixel.y >= frame.height {
                        continue;
                    }
                    let idx = (pixel.y as usize * w + pixel.x as usize) * 4;
                    blend_rgba(&mut rgba[idx..idx + 4], *color);
                }
            }
            Overlay::CrosshairMarkers {
                markers,
                color,
                arm_len,
            } => {
                for marker in markers {
                    draw_crosshair(
                        &mut rgba,
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

    ColorImage::from_rgba_unmultiplied([w, h], &rgba)
}

#[cfg(test)]
fn percentile_value(pixels: &[u16], percentile: f32) -> u16 {
    percentile_value_from_slices(&[pixels], percentile)
}

fn percentile_value_two(on: &[u16], off: &[u16], percentile: f32) -> u16 {
    percentile_value_from_slices(&[on, off], percentile)
}

fn percentile_value_from_slices(channels: &[&[u16]], percentile: f32) -> u16 {
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
    let mut hist = vec![0u32; hist_size];
    for channel in channels {
        for &value in *channel {
            hist[(value as usize).min(hist_size - 1)] += 1;
        }
    }

    let percentile = percentile.clamp(0.0, 100.0);
    let len = channels.iter().map(|channel| channel.len()).sum::<usize>();
    let target =
        ((len as f64 * percentile as f64 / 100.0).ceil() as u64).clamp(1, len.max(1) as u64);
    let mut cumulative = 0u64;
    for (value, &count) in hist.iter().enumerate() {
        cumulative += u64::from(count);
        if cumulative >= target {
            return value.max(1) as u16;
        }
    }

    hist_size.max(1) as u16
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
fn render_base_with_grid(
    frame: &PreviewFrame,
    grid: &RoiGrid,
    top_n: usize,
    max: f32,
    w: usize,
    h: usize,
    rgba: &mut [u8],
) {
    let grid_cols = grid.cols();
    let grid_rows = grid.rows();

    // O(W + H): map each pixel coordinate to its grid cell index.
    let col_map = build_pixel_to_grid_map(&grid.x_bounds, w);
    let row_map = build_pixel_to_grid_map(&grid.y_bounds, h);

    // O(W + H): boolean flags for grid line positions.
    let mut x_on_line = vec![false; w];
    for &xb in &grid.x_bounds {
        let xb = xb as usize;
        if xb > 0 && xb < w {
            x_on_line[xb] = true;
        }
    }
    let mut y_on_line = vec![false; h];
    for &yb in &grid.y_bounds {
        let yb = yb as usize;
        if yb > 0 && yb < h {
            y_on_line[yb] = true;
        }
    }

    // Mark grid cells inside top-N highlight rectangles.
    let n = top_n.min(grid.largest_rects.len());
    let mut cell_highlighted = vec![false; grid_rows * grid_cols];
    for rect in &grid.largest_rects[..n] {
        let c0 = grid_bound_index(&grid.x_bounds, rect.x);
        let c1 = grid_bound_index(&grid.x_bounds, rect.x + rect.width).min(grid_cols);
        let r0 = grid_bound_index(&grid.y_bounds, rect.y);
        let r1 = grid_bound_index(&grid.y_bounds, rect.y + rect.height).min(grid_rows);
        for r in r0..r1 {
            for c in c0..c1 {
                cell_highlighted[r * grid_cols + c] = true;
            }
        }
    }

    // Tint colors as u32 arrays for arithmetic.
    const LINE: [u32; 4] = [0, 220, 220, 100];
    const HIGHLIGHT: [u32; 4] = [80, 255, 120, 72];
    const FREE: [u32; 4] = [40, 220, 80, 36];
    const BLOCKED: [u32; 4] = [255, 40, 40, 80];

    // Single sequential pass — perfect cache locality, no per-cell function calls.
    for py in 0..h {
        let gr = row_map[py];
        let on_hline = y_on_line[py];
        let row_off = py * w;

        for px in 0..w {
            let on_v = frame.pixels_on[row_off + px];
            let off_v = frame.pixels_off[row_off + px];
            let base_g = u32::from(normalized_channel(on_v, max).max(8));
            let base_r = u32::from(normalized_channel(off_v, max).max(8));

            let gc = col_map[px];
            let cell = gr * grid_cols + gc;

            let tint = if on_hline || x_on_line[px] {
                LINE
            } else if cell_highlighted[cell] {
                HIGHLIGHT
            } else if grid.blocked[cell] {
                BLOCKED
            } else {
                FREE
            };

            let a = tint[3];
            let inv = 255 - a;
            let off = (row_off + px) * 4;
            rgba[off] = ((base_r * inv + tint[0] * a) / 255) as u8;
            rgba[off + 1] = ((base_g * inv + tint[1] * a) / 255) as u8;
            rgba[off + 2] = ((8 * inv + tint[2] * a) / 255) as u8;
            rgba[off + 3] = 255;
        }
    }

    // Yellow borders for highlight rects — thin lines, negligible cost.
    let border_color = [255u8, 220, 0, 200];
    for rect in &grid.largest_rects[..n] {
        draw_rect_border(
            rgba,
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
fn build_pixel_to_grid_map(bounds: &[u16], size: usize) -> Vec<usize> {
    let max_idx = bounds.len().saturating_sub(2);
    let mut map = vec![0usize; size];
    let mut gi = 0;
    for (px, slot) in map.iter_mut().enumerate() {
        while gi < max_idx && (px as u16) >= bounds[gi + 1] {
            gi += 1;
        }
        *slot = gi;
    }
    map
}

/// Find the boundary index for a grid-aligned value.
fn grid_bound_index(bounds: &[u16], val: u16) -> usize {
    bounds.partition_point(|&b| b < val)
}

fn blend_rgba(dst: &mut [u8], src: [u8; 4]) {
    let alpha = u32::from(src[3]);
    let inv_alpha = 255 - alpha;

    for channel in 0..3 {
        let blended = (u32::from(dst[channel]) * inv_alpha + u32::from(src[channel]) * alpha) / 255;
        dst[channel] = blended as u8;
    }
    dst[3] = 255;
}

fn draw_crosshair(
    rgba: &mut [u8],
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
        blend_pixel(rgba, width, height, cx + dx, cy, color);
    }
    for dy in -arm_len..=arm_len {
        if dy.abs() <= 1 {
            continue;
        }
        blend_pixel(rgba, width, height, cx, cy + dy, color);
    }
    blend_pixel(rgba, width, height, cx, cy, color);
}

fn blend_pixel(rgba: &mut [u8], width: usize, height: usize, x: isize, y: isize, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
        return;
    }
    let idx = (y as usize * width + x as usize) * 4;
    blend_rgba(&mut rgba[idx..idx + 4], color);
}

fn draw_rect_border(
    rgba: &mut [u8],
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
                let idx = ((rect.y + t) * width + px) * 4;
                blend_rgba(&mut rgba[idx..idx + 4], color);
            }
        }
        if y_end > t && y_end - 1 - t >= rect.y {
            let by = y_end - 1 - t;
            for px in rect.x..x_end {
                let idx = (by * width + px) * 4;
                blend_rgba(&mut rgba[idx..idx + 4], color);
            }
        }
    }
    // Left and right edges — skip rows already covered by the top/bottom passes.
    let inner_y = (rect.y + thickness).min(y_end);
    let inner_y_end = y_end.saturating_sub(thickness).max(inner_y);
    for py in inner_y..inner_y_end {
        for t in 0..thickness {
            if rect.x + t < x_end {
                let idx = (py * width + rect.x + t) * 4;
                blend_rgba(&mut rgba[idx..idx + 4], color);
            }
            if x_end > t && x_end - 1 - t >= rect.x {
                let rx = x_end - 1 - t;
                let idx = (py * width + rx) * 4;
                blend_rgba(&mut rgba[idx..idx + 4], color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{percentile_value, percentile_value_two};

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
}
