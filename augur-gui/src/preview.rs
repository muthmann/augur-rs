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

pub fn frame_to_color_image(frame: &PreviewFrame, overlays: &[Overlay]) -> ColorImage {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let max = frame.pixels.iter().copied().max().unwrap_or(1) as f32;
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
        for (i, &v) in frame.pixels.iter().enumerate() {
            let g = ((v as f32 / max).sqrt() * 255.0) as u8;
            let off = i * 4;
            rgba[off] = 8;
            rgba[off + 1] = g;
            rgba[off + 2] = 8;
            rgba[off + 3] = 255;
        }
    }

    // Sparse overlays (HighlightPixels) as a separate pass — only touches marked pixels.
    for overlay in overlays {
        if let Overlay::HighlightPixels { pixels, color } = overlay {
            for pixel in pixels {
                if pixel.x >= frame.width || pixel.y >= frame.height {
                    continue;
                }
                let idx = (pixel.y as usize * w + pixel.x as usize) * 4;
                blend_rgba(&mut rgba[idx..idx + 4], *color);
            }
        }
    }

    ColorImage::from_rgba_unmultiplied([w, h], &rgba)
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
            let v = frame.pixels[row_off + px];
            let g = ((v as f32 / max).sqrt() * 255.0) as u32;

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
            rgba[off] = ((8 * inv + tint[0] * a) / 255) as u8;
            rgba[off + 1] = ((g * inv + tint[1] * a) / 255) as u8;
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
        let blended =
            (u32::from(dst[channel]) * inv_alpha + u32::from(src[channel]) * alpha) / 255;
        dst[channel] = blended as u8;
    }
    dst[3] = 255;
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
    // Left and right edges.
    for py in rect.y..y_end {
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
