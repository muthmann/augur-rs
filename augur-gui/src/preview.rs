use augur_core::{analysis::Overlay, pipeline::PreviewFrame};
use egui::ColorImage;

#[derive(Clone, Copy)]
struct PixelRect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

pub fn frame_to_color_image(frame: &PreviewFrame, overlays: &[Overlay]) -> ColorImage {
    let max = frame.pixels.iter().copied().max().unwrap_or(1) as f32;
    let mut rgba = Vec::with_capacity(frame.pixels.len() * 4);

    for &v in &frame.pixels {
        let norm = (v as f32 / max).sqrt();
        let g = (norm * 255.0) as u8;
        rgba.extend_from_slice(&[8, g, 8, 255]);
    }

    if !overlays.is_empty() {
        for overlay in overlays {
            match overlay {
                Overlay::HighlightPixels { pixels, color } => {
                    for pixel in pixels {
                        if pixel.x >= frame.width || pixel.y >= frame.height {
                            continue;
                        }
                        let idx = (pixel.y as usize * frame.width as usize + pixel.x as usize) * 4;
                        blend_rgba(&mut rgba[idx..idx + 4], *color);
                    }
                }
                Overlay::RoiGrid {
                    grid,
                    highlight_top_n,
                } => {
                    let size = [frame.width as usize, frame.height as usize];

                    // Tint free cells with a subtle green wash so the partitioned
                    // regions remain visible across the preview.
                    let free_tint = [40, 220, 80, 36];
                    for (r, row) in grid.blocked.iter().enumerate() {
                        for (c, &is_blocked) in row.iter().enumerate() {
                            if !is_blocked {
                                fill_rect(
                                    &mut rgba,
                                    size,
                                    PixelRect {
                                        x: grid.x_bounds[c] as usize,
                                        y: grid.y_bounds[r] as usize,
                                        width: (grid.x_bounds[c + 1] - grid.x_bounds[c]) as usize,
                                        height: (grid.y_bounds[r + 1] - grid.y_bounds[r]) as usize,
                                    },
                                    free_tint,
                                );
                            }
                        }
                    }

                    // Tint blocked cells red (at most 64 hotpixels, each 1x1).
                    let red_tint = [255, 40, 40, 80];
                    for (r, row) in grid.blocked.iter().enumerate() {
                        for (c, &is_blocked) in row.iter().enumerate() {
                            if is_blocked {
                                fill_rect(
                                    &mut rgba,
                                    size,
                                    PixelRect {
                                        x: grid.x_bounds[c] as usize,
                                        y: grid.y_bounds[r] as usize,
                                        width: (grid.x_bounds[c + 1] - grid.x_bounds[c]) as usize,
                                        height: (grid.y_bounds[r + 1] - grid.y_bounds[r]) as usize,
                                    },
                                    red_tint,
                                );
                            }
                        }
                    }

                    // Draw grid lines (cyan, semi-transparent).
                    let cyan = [0, 220, 220, 100];
                    for &x in &grid.x_bounds {
                        if x > 0 && (x as usize) < size[0] {
                            draw_vline(&mut rgba, size[0], size[1], x as usize, cyan);
                        }
                    }
                    for &y in &grid.y_bounds {
                        if y > 0 && (y as usize) < size[1] {
                            draw_hline(&mut rgba, size[0], size[1], y as usize, cyan);
                        }
                    }

                    // Highlight top-N largest rectangles with a stronger fill and
                    // border so the best ROI candidates stand out.
                    let highlight_fill = [80, 255, 120, 72];
                    let yellow_border = [255, 220, 0, 200];
                    let n = (*highlight_top_n).min(grid.largest_rects.len());
                    for rect in &grid.largest_rects[..n] {
                        fill_rect(
                            &mut rgba,
                            size,
                            PixelRect {
                                x: rect.x as usize,
                                y: rect.y as usize,
                                width: rect.width as usize,
                                height: rect.height as usize,
                            },
                            highlight_fill,
                        );
                        draw_rect_border(
                            &mut rgba,
                            size,
                            PixelRect {
                                x: rect.x as usize,
                                y: rect.y as usize,
                                width: rect.width as usize,
                                height: rect.height as usize,
                            },
                            2,
                            yellow_border,
                        );
                    }
                }
            }
        }
    }

    ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &rgba)
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

fn fill_rect(rgba: &mut [u8], size: [usize; 2], rect: PixelRect, color: [u8; 4]) {
    let [width, height] = size;
    let x_end = (rect.x + rect.width).min(width);
    let y_end = (rect.y + rect.height).min(height);
    for py in rect.y..y_end {
        for px in rect.x..x_end {
            let idx = (py * width + px) * 4;
            blend_rgba(&mut rgba[idx..idx + 4], color);
        }
    }
}

fn draw_hline(rgba: &mut [u8], w: usize, h: usize, y: usize, color: [u8; 4]) {
    if y >= h {
        return;
    }
    for px in 0..w {
        let idx = (y * w + px) * 4;
        blend_rgba(&mut rgba[idx..idx + 4], color);
    }
}

fn draw_vline(rgba: &mut [u8], w: usize, h: usize, x: usize, color: [u8; 4]) {
    if x >= w {
        return;
    }
    for py in 0..h {
        let idx = (py * w + x) * 4;
        blend_rgba(&mut rgba[idx..idx + 4], color);
    }
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
