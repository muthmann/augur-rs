use std::cell::RefCell;

use augur_core::{
    analysis::{roi_grid::RoiGrid, Overlay},
    pipeline::PreviewFrame,
};
use egui::{Color32, ColorImage};

use crate::colormap::Colormap;

thread_local! {
    static PREVIEW_SCRATCH: RefCell<PreviewRenderScratch> = RefCell::new(PreviewRenderScratch::default());
}

#[derive(Default)]
struct PreviewRenderScratch {
    grid_col_map: Vec<usize>,
    grid_row_map: Vec<usize>,
    grid_x_line: Vec<bool>,
    grid_y_line: Vec<bool>,
    grid_cell_highlight: Vec<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewDisplaySettings {
    pub display_min: u16,
    pub display_max: u16,
    pub gamma: f32,
}

impl Default for PreviewDisplaySettings {
    fn default() -> Self {
        Self {
            display_min: 0,
            display_max: 1,
            gamma: 0.5,
        }
    }
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
    settings: PreviewDisplaySettings,
    colormap: Option<Colormap>,
) -> ColorImage {
    let w = frame.width as usize;
    let h = frame.height as usize;
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
                settings,
                colormap,
                [w, h],
                &mut image.pixels[..],
                &mut scratch,
            );
        });
    } else {
        render_base(frame, settings, colormap, &mut image.pixels);
    }

    render_overlays(frame, overlays, &mut image.pixels, w, h);

    image
}

pub fn compute_frame_histogram(frame: &PreviewFrame) -> Vec<u64> {
    let max_value = frame.pixels.iter().copied().max().unwrap_or(0) as usize;
    if max_value == 0 {
        return vec![frame.pixels.len() as u64];
    }

    let mut histogram = vec![0u64; max_value + 1];
    for &value in &frame.pixels {
        histogram[value as usize] += 1;
    }
    histogram
}

fn render_base(
    frame: &PreviewFrame,
    settings: PreviewDisplaySettings,
    colormap: Option<Colormap>,
    pixels: &mut [Color32],
) {
    for (i, pixel) in pixels.iter_mut().enumerate() {
        *pixel = preview_pixel_color(frame, i, settings, colormap);
    }
}

fn normalized_value(value: u16, settings: PreviewDisplaySettings) -> f32 {
    let display_min = settings
        .display_min
        .min(settings.display_max.saturating_sub(1));
    let display_max = settings.display_max.max(display_min.saturating_add(1));
    let numerator = (f32::from(value) - f32::from(display_min)).max(0.0);
    let denominator = (f32::from(display_max) - f32::from(display_min)).max(1.0);
    (numerator / denominator)
        .clamp(0.0, 1.0)
        .powf(settings.gamma.max(0.01))
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
    settings: PreviewDisplaySettings,
    colormap: Option<Colormap>,
    size: [usize; 2],
    pixels: &mut [Color32],
    scratch: &mut PreviewRenderScratch,
) {
    let [w, h] = size;
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
            let [base_r, base_g, base_b, _] =
                preview_pixel_color(frame, row_off + px, settings, colormap).to_array();
            let base_r = u32::from(base_r);
            let base_g = u32::from(base_g);
            let base_b = u32::from(base_b);

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
            let b = ((base_b * inv + tint[2] * a) / 255) as u8;
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

fn preview_pixel_color(
    frame: &PreviewFrame,
    index: usize,
    settings: PreviewDisplaySettings,
    colormap: Option<Colormap>,
) -> Color32 {
    match colormap {
        Some(colormap) => colormap.lookup(normalized_value(frame.pixels[index], settings)),
        None => polarity_color(
            normalized_value(frame.pixels_on[index], settings),
            normalized_value(frame.pixels_off[index], settings),
        ),
    }
}

fn polarity_color(on_value: f32, off_value: f32) -> Color32 {
    Color32::from_rgb(
        channel_to_u8(off_value).max(8),
        channel_to_u8(on_value).max(8),
        8,
    )
}

fn channel_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
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
    use super::{compute_frame_histogram, frame_to_color_image, PreviewDisplaySettings};
    use crate::colormap::Colormap;
    use augur_core::{
        analysis::{roi_grid::compute_roi_grid, Overlay, Pixel, SubpixelMarker},
        pipeline::PreviewFrame,
    };
    use std::sync::Arc;

    #[test]
    fn histogram_counts_combined_pixels() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![0, 2, 2],
            pixels_on: vec![0, 1, 2],
            pixels_off: vec![0, 1, 0],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };
        let histogram = compute_frame_histogram(&frame);
        assert_eq!(histogram, vec![1, 0, 2]);
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

        let image =
            frame_to_color_image(&frame, &overlays, PreviewDisplaySettings::default(), None);

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

        let image =
            frame_to_color_image(&frame, &overlays, PreviewDisplaySettings::default(), None);

        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels.len(), 4);
    }

    #[test]
    fn false_color_preview_uses_shared_lookup_tables() {
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![0],
            pixels_off: vec![0],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };

        let image = frame_to_color_image(
            &frame,
            &[],
            PreviewDisplaySettings::default(),
            Some(Colormap::Green),
        );

        assert_eq!(image.pixels[0], Colormap::Green.lookup(1.0));
    }
}
