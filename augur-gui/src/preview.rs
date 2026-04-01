use std::cell::RefCell;

use augur_core::{
    analysis::{roi_grid::RoiGrid, Overlay},
    pipeline::{CdEvent, PreviewFrame},
};
use egui::{Color32, ColorImage};

use crate::colormap::Colormap;

thread_local! {
    static PREVIEW_SCRATCH: RefCell<PreviewRenderScratch> = RefCell::new(PreviewRenderScratch::default());
}

const MAX_HISTOGRAM_BINS: usize = 4096;
const TIME_SURFACE_BINS: usize = 256;

#[derive(Default)]
struct PreviewRenderScratch {
    grid_col_map: Vec<usize>,
    grid_row_map: Vec<usize>,
    grid_x_line: Vec<bool>,
    grid_y_line: Vec<bool>,
    grid_cell_highlight: Vec<bool>,
    time_surface: Vec<u64>,
    time_surface_size: [usize; 2],
    time_surface_frame_end_us: Option<u64>,
    time_surface_values: Vec<u8>,
    time_surface_decay_key: Option<TimeSurfaceDecayKey>,
    time_surface_histogram: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimeSurfaceDecayKey {
    frame_end_us: u64,
    tau_us: u64,
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
            display_max: 255,
            gamma: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DisplayNormalizer {
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
}

impl DisplayNormalizer {
    fn new(settings: PreviewDisplaySettings) -> Self {
        let display_min = settings
            .display_min
            .min(settings.display_max.saturating_sub(1));
        let display_max = settings.display_max.max(display_min.saturating_add(1));
        let range = (f32::from(display_max) - f32::from(display_min)).max(1.0);
        Self {
            display_min: f32::from(display_min),
            inverse_range: 1.0 / range,
            gamma: settings.gamma.max(0.01),
        }
    }

    fn normalize(self, value: u16) -> f32 {
        ((f32::from(value) - self.display_min).max(0.0) * self.inverse_range)
            .clamp(0.0, 1.0)
            .powf(self.gamma)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    RedBlue,
    SignedCount,
    Intensity(Colormap),
    TimeSurface,
}

impl Default for PreviewMode {
    fn default() -> Self {
        Self::Intensity(Colormap::Grays)
    }
}

impl PreviewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::RedBlue => "Red-Blue Polarity",
            Self::SignedCount => "Signed Count",
            Self::Intensity(colormap) => colormap.label(),
            Self::TimeSurface => "Time Surface",
        }
    }

    pub fn requires_raw_events(self) -> bool {
        matches!(self, Self::TimeSurface)
    }

    pub fn ramp_label(self) -> &'static str {
        match self {
            Self::RedBlue => "Red-blue polarity ramp",
            Self::SignedCount => "Signed-count diverging ramp",
            Self::Intensity(_) => "Intensity display ramp",
            Self::TimeSurface => "Time-surface decay ramp",
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

pub fn reset_preview_render_cache() {
    PREVIEW_SCRATCH.with(|scratch| {
        *scratch.borrow_mut() = PreviewRenderScratch::default();
    });
}

pub fn query_time_surface_value(index: usize) -> Option<u8> {
    PREVIEW_SCRATCH.with(|scratch| {
        let scratch = scratch.borrow();
        scratch
            .time_surface_values
            .get(index)
            .copied()
            .filter(|_| scratch.time_surface_decay_key.is_some())
    })
}

pub fn frame_to_color_image(
    frame: &PreviewFrame,
    overlays: &[Overlay],
    settings: PreviewDisplaySettings,
    mode: PreviewMode,
    time_surface_tau_us: u64,
) -> ColorImage {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let normalizer = DisplayNormalizer::new(settings);
    let mut image = ColorImage::new([w, h], Color32::TRANSPARENT);

    // Look for a ROI grid overlay to merge into the base rendering pass.
    let roi = overlays.iter().find_map(|o| match o {
        Overlay::RoiGrid {
            grid,
            highlight_top_n,
        } => Some((grid.as_ref(), *highlight_top_n)),
        _ => None,
    });

    PREVIEW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        if let Some((grid, top_n)) = roi {
            render_base_with_grid(
                frame,
                grid,
                top_n,
                normalizer,
                mode,
                time_surface_tau_us,
                [w, h],
                &mut image.pixels[..],
                &mut scratch,
            );
        } else {
            render_base(
                frame,
                normalizer,
                mode,
                time_surface_tau_us,
                &mut image.pixels,
                &mut scratch,
            );
        }
    });

    render_overlays(frame, overlays, &mut image.pixels, w, h);

    image
}

pub fn compute_frame_histogram(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
) -> Vec<u64> {
    match mode {
        PreviewMode::RedBlue | PreviewMode::Intensity(_) => {
            histogram_from_values(frame.pixels.iter().copied())
        }
        PreviewMode::SignedCount => histogram_from_values(
            frame
                .pixels_on
                .iter()
                .zip(&frame.pixels_off)
                .map(|(&on, &off)| on.abs_diff(off)),
        ),
        PreviewMode::TimeSurface => PREVIEW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            if !ensure_time_surface_render_cache(frame, time_surface_tau_us, &mut scratch) {
                return histogram_from_values(frame.pixels.iter().copied());
            }
            scratch.time_surface_histogram.clone()
        }),
    }
}

fn render_base(
    frame: &PreviewFrame,
    normalizer: DisplayNormalizer,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    pixels: &mut [Color32],
    scratch: &mut PreviewRenderScratch,
) {
    prepare_time_surface(frame, mode, time_surface_tau_us, scratch);
    for (index, pixel) in pixels.iter_mut().enumerate() {
        *pixel = preview_pixel_color(frame, index, normalizer, mode, time_surface_tau_us, scratch);
    }
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
    normalizer: DisplayNormalizer,
    mode: PreviewMode,
    time_surface_tau_us: u64,
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
        for row in r0..r1 {
            for col in c0..c1 {
                scratch.grid_cell_highlight[row * grid_cols + col] = true;
            }
        }
    }

    prepare_time_surface(frame, mode, time_surface_tau_us, scratch);

    // Tint colors as u32 arrays for arithmetic.
    const LINE: [u32; 4] = [0, 220, 220, 100];
    const HIGHLIGHT: [u32; 4] = [80, 255, 120, 72];
    const FREE: [u32; 4] = [40, 220, 80, 36];
    const BLOCKED: [u32; 4] = [255, 40, 40, 80];

    // Single sequential pass - perfect cache locality, no per-cell function calls.
    for py in 0..h {
        let grid_row = scratch.grid_row_map[py];
        let on_hline = scratch.grid_y_line[py];
        let row_offset = py * w;

        for px in 0..w {
            let [base_r, base_g, base_b, _] = preview_pixel_color(
                frame,
                row_offset + px,
                normalizer,
                mode,
                time_surface_tau_us,
                scratch,
            )
            .to_array();
            let base_r = u32::from(base_r);
            let base_g = u32::from(base_g);
            let base_b = u32::from(base_b);

            let grid_col = scratch.grid_col_map[px];
            let cell = grid_row * grid_cols + grid_col;

            let tint = if on_hline || scratch.grid_x_line[px] {
                LINE
            } else if scratch.grid_cell_highlight[cell] {
                HIGHLIGHT
            } else if grid.blocked[cell] {
                BLOCKED
            } else {
                FREE
            };

            let alpha = tint[3];
            let inv_alpha = 255 - alpha;
            let r = ((base_r * inv_alpha + tint[0] * alpha) / 255) as u8;
            let g = ((base_g * inv_alpha + tint[1] * alpha) / 255) as u8;
            let b = ((base_b * inv_alpha + tint[2] * alpha) / 255) as u8;
            pixels[row_offset + px] = Color32::from_rgb(r, g, b);
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
    normalizer: DisplayNormalizer,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    scratch: &PreviewRenderScratch,
) -> Color32 {
    match mode {
        PreviewMode::RedBlue => polarity_red_blue(frame, index, normalizer),
        PreviewMode::SignedCount => signed_count_color(frame, index, normalizer),
        PreviewMode::Intensity(colormap) => {
            colormap.lookup(normalizer.normalize(frame.pixels[index]))
        }
        PreviewMode::TimeSurface => {
            time_surface_color(frame, index, normalizer, time_surface_tau_us, scratch)
                .unwrap_or_else(|| {
                    Colormap::Grays.lookup(normalizer.normalize(frame.pixels[index]))
                })
        }
    }
}

fn prepare_time_surface(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    scratch: &mut PreviewRenderScratch,
) {
    if matches!(mode, PreviewMode::TimeSurface) {
        let _ = ensure_time_surface_render_cache(frame, time_surface_tau_us, scratch);
    }
}

fn ensure_time_surface_state(frame: &PreviewFrame, scratch: &mut PreviewRenderScratch) -> bool {
    let size = [frame.width as usize, frame.height as usize];
    let pixel_count = size[0].saturating_mul(size[1]);
    let geometry_changed =
        scratch.time_surface_size != size || scratch.time_surface.len() != pixel_count;
    let timestamp_regressed = scratch
        .time_surface_frame_end_us
        .is_some_and(|last_end| frame.window_end_us < last_end);

    if geometry_changed || timestamp_regressed {
        scratch.time_surface.resize(pixel_count, 0);
        scratch.time_surface.fill(0);
        scratch.time_surface_size = size;
        scratch.time_surface_frame_end_us = None;
        scratch.time_surface_values.resize(pixel_count, 0);
        scratch.time_surface_values.fill(0);
        scratch.time_surface_decay_key = None;
        scratch.time_surface_histogram.resize(TIME_SURFACE_BINS, 0);
        scratch.time_surface_histogram.fill(0);
    }

    if scratch.time_surface_frame_end_us == Some(frame.window_end_us) {
        return true;
    }

    let Some(events) = frame.events.as_deref() else {
        return false;
    };
    update_time_surface(events, frame.width, frame.height, &mut scratch.time_surface);
    scratch.time_surface_frame_end_us = Some(frame.window_end_us);
    scratch.time_surface_decay_key = None;
    true
}

fn ensure_time_surface_render_cache(
    frame: &PreviewFrame,
    time_surface_tau_us: u64,
    scratch: &mut PreviewRenderScratch,
) -> bool {
    if !ensure_time_surface_state(frame, scratch) {
        return false;
    }

    let key = TimeSurfaceDecayKey {
        frame_end_us: frame.window_end_us,
        tau_us: time_surface_tau_us.max(1),
    };
    let pixel_count = frame.width as usize * frame.height as usize;
    if scratch.time_surface_decay_key == Some(key)
        && scratch.time_surface_values.len() == pixel_count
    {
        return true;
    }

    scratch.time_surface_values.resize(pixel_count, 0);
    scratch.time_surface_histogram.resize(TIME_SURFACE_BINS, 0);
    scratch.time_surface_histogram.fill(0);
    for (index, &timestamp) in scratch.time_surface.iter().enumerate() {
        let value = time_surface_value_u8(timestamp, frame.window_end_us, key.tau_us);
        scratch.time_surface_values[index] = value;
        scratch.time_surface_histogram[usize::from(value)] += 1;
    }
    scratch.time_surface_decay_key = Some(key);
    true
}

fn update_time_surface(events: &[CdEvent], width: u16, height: u16, time_surface: &mut [u64]) {
    for event in events {
        if event.x >= width || event.y >= height {
            continue;
        }
        let index = event.y as usize * width as usize + event.x as usize;
        let encoded_timestamp = event.timestamp.saturating_add(1);
        time_surface[index] = time_surface[index].max(encoded_timestamp);
    }
}

fn polarity_red_blue(frame: &PreviewFrame, index: usize, normalizer: DisplayNormalizer) -> Color32 {
    let total = frame.pixels[index];
    if total == 0 {
        return Color32::BLACK;
    }

    let on = frame.pixels_on[index];
    let off = frame.pixels_off[index];
    let brightness = channel_to_u8(normalizer.normalize(total));
    match on.cmp(&off) {
        std::cmp::Ordering::Greater => Color32::from_rgb(brightness, 0, 0),
        std::cmp::Ordering::Less => Color32::from_rgb(0, 0, brightness),
        std::cmp::Ordering::Equal => Color32::from_rgb(brightness, 0, brightness),
    }
}

fn signed_count_color(
    frame: &PreviewFrame,
    index: usize,
    normalizer: DisplayNormalizer,
) -> Color32 {
    if frame.pixels[index] == 0 {
        return Color32::BLACK;
    }

    let on = frame.pixels_on[index];
    let off = frame.pixels_off[index];
    let magnitude = normalizer.normalize(on.abs_diff(off));
    let signed_t = match on.cmp(&off) {
        std::cmp::Ordering::Greater => 0.5 + 0.5 * magnitude,
        std::cmp::Ordering::Less => 0.5 - 0.5 * magnitude,
        std::cmp::Ordering::Equal => 0.5,
    };
    Colormap::BlueWhiteRed.lookup(signed_t)
}

fn time_surface_color(
    frame: &PreviewFrame,
    index: usize,
    normalizer: DisplayNormalizer,
    time_surface_tau_us: u64,
    scratch: &PreviewRenderScratch,
) -> Option<Color32> {
    if scratch.time_surface_decay_key
        != Some(TimeSurfaceDecayKey {
            frame_end_us: frame.window_end_us,
            tau_us: time_surface_tau_us.max(1),
        })
    {
        return None;
    }

    Some(
        Colormap::Grays.lookup(normalizer.normalize(u16::from(scratch.time_surface_values[index]))),
    )
}

fn time_surface_value_u8(timestamp: u64, reference_us: u64, tau_us: u64) -> u8 {
    if timestamp == 0 {
        return 0;
    }

    let last_event_us = timestamp.saturating_sub(1);
    let dt = reference_us.saturating_sub(last_event_us);
    let tau_us = tau_us.max(1) as f64;
    let value = (-(dt as f64) / tau_us).exp();
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn histogram_from_values(values: impl IntoIterator<Item = u16>) -> Vec<u64> {
    let mut histogram = Vec::new();
    for value in values {
        let index = (value as usize).min(MAX_HISTOGRAM_BINS - 1);
        if histogram.len() <= index {
            histogram.resize(index + 1, 0);
        }
        histogram[index] += 1;
    }
    if histogram.is_empty() {
        histogram.push(0);
    }
    histogram
}

fn channel_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Linear sweep mapping each pixel coordinate to its grid cell index.
fn build_pixel_to_grid_map(bounds: &[u16], size: usize, out: &mut Vec<usize>) {
    out.resize(size, 0);
    let max_idx = bounds.len().saturating_sub(2);
    let mut grid_index = 0;
    for (pixel, slot) in out.iter_mut().enumerate() {
        while grid_index < max_idx && (pixel as u16) >= bounds[grid_index + 1] {
            grid_index += 1;
        }
        *slot = grid_index;
    }
}

/// Find the boundary index for a grid-aligned value.
fn grid_bound_index(bounds: &[u16], val: u16) -> usize {
    bounds.partition_point(|&bound| bound < val)
}

fn blend_color(dst: &mut Color32, src: [u8; 4]) {
    let [dr, dg, db, _] = dst.to_array();
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
    let index = y as usize * width + x as usize;
    blend_color(&mut pixels[index], color);
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
    for offset in 0..thickness {
        if rect.y + offset < y_end {
            for px in rect.x..x_end {
                let index = (rect.y + offset) * width + px;
                blend_color(&mut pixels[index], color);
            }
        }
        if y_end > offset && y_end - 1 - offset >= rect.y {
            let by = y_end - 1 - offset;
            for px in rect.x..x_end {
                let index = by * width + px;
                blend_color(&mut pixels[index], color);
            }
        }
    }
    // Left and right edges - skip rows already covered by the top/bottom passes.
    let inner_y = (rect.y + thickness).min(y_end);
    let inner_y_end = y_end.saturating_sub(thickness).max(inner_y);
    for py in inner_y..inner_y_end {
        for offset in 0..thickness {
            if rect.x + offset < x_end {
                let index = py * width + rect.x + offset;
                blend_color(&mut pixels[index], color);
            }
            if x_end > offset && x_end - 1 - offset >= rect.x {
                let rx = x_end - 1 - offset;
                let index = py * width + rx;
                blend_color(&mut pixels[index], color);
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
                    let index = pixel.y as usize * w + pixel.x as usize;
                    blend_color(&mut pixels[index], *color);
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
    use super::{
        compute_frame_histogram, frame_to_color_image, query_time_surface_value,
        reset_preview_render_cache, PreviewDisplaySettings, PreviewMode, MAX_HISTOGRAM_BINS,
        TIME_SURFACE_BINS,
    };
    use crate::colormap::Colormap;
    use augur_core::{
        analysis::{roi_grid::compute_roi_grid, Overlay, Pixel, SubpixelMarker},
        pipeline::{CdEvent, PreviewFrame},
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
        let histogram =
            compute_frame_histogram(&frame, PreviewMode::Intensity(Colormap::Grays), 30_000);
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

        let image = frame_to_color_image(
            &frame,
            &overlays,
            PreviewDisplaySettings::default(),
            PreviewMode::RedBlue,
            30_000,
        );

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

        let image = frame_to_color_image(
            &frame,
            &overlays,
            PreviewDisplaySettings::default(),
            PreviewMode::RedBlue,
            30_000,
        );

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
            PreviewDisplaySettings {
                display_min: 0,
                display_max: 1,
                gamma: 1.0,
            },
            PreviewMode::Intensity(Colormap::Green),
            30_000,
        );

        assert_eq!(image.pixels[0], Colormap::Green.lookup(1.0));
    }

    #[test]
    fn signed_count_histogram_uses_net_polarity_magnitude() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![4, 6, 3],
            pixels_on: vec![4, 2, 1],
            pixels_off: vec![0, 5, 1],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };

        let histogram = compute_frame_histogram(&frame, PreviewMode::SignedCount, 30_000);
        assert_eq!(histogram, vec![1, 0, 0, 1, 1]);
    }

    #[test]
    fn time_surface_histogram_uses_decayed_values_when_events_are_available() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 10,
                polarity: true,
            }]),
            window_start_us: 0,
            window_end_us: 10,
        };

        let histogram = compute_frame_histogram(&frame, PreviewMode::TimeSurface, 30_000);
        assert_eq!(histogram.len(), TIME_SURFACE_BINS);
        assert_eq!(histogram[255], 1);
    }

    #[test]
    fn query_time_surface_value_returns_cached_decay_values() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 10,
                polarity: true,
            }]),
            window_start_us: 0,
            window_end_us: 10,
        };

        assert_eq!(query_time_surface_value(0), None);

        let _ = compute_frame_histogram(&frame, PreviewMode::TimeSurface, 30_000);

        assert_eq!(query_time_surface_value(0), Some(255));
        assert_eq!(query_time_surface_value(1), None);
    }

    #[test]
    fn histogram_caps_hot_bins_to_keep_ui_work_bounded() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![0, 4095, 5000],
            pixels_on: vec![0, 4095, 5000],
            pixels_off: vec![0, 0, 0],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 1,
        };

        let histogram =
            compute_frame_histogram(&frame, PreviewMode::Intensity(Colormap::Grays), 30_000);
        assert_eq!(histogram.len(), MAX_HISTOGRAM_BINS);
        assert_eq!(histogram[0], 1);
        assert_eq!(histogram[MAX_HISTOGRAM_BINS - 1], 2);
    }
}
