use std::sync::{Arc, Mutex};

use augur_core::pipeline::PreviewFrame;
use egui::Color32;
use egui_plot::{Legend, Line, Plot};

const LINE_PROFILE_VIEWPORT_TITLE: &str = "Line Profile - AugurRS";

#[derive(Debug)]
pub struct LineProfileTool {
    pub start: Option<(u16, u16)>,
    pub end: Option<(u16, u16)>,
    pub profile_on: Vec<f32>,
    pub profile_off: Vec<f32>,
    pub window_open: bool,
    shared: Arc<Mutex<LineProfileViewportData>>,
}

impl Default for LineProfileTool {
    fn default() -> Self {
        Self {
            start: None,
            end: None,
            profile_on: Vec::new(),
            profile_off: Vec::new(),
            window_open: false,
            shared: Arc::new(Mutex::new(LineProfileViewportData::default())),
        }
    }
}

#[derive(Debug, Default)]
struct LineProfileViewportData {
    profile_on: Vec<f32>,
    profile_off: Vec<f32>,
    show_sum: bool,
    close_requested: bool,
}

impl LineProfileTool {
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.profile_on.clear();
        self.profile_off.clear();
        self.window_open = false;
        if let Ok(mut data) = self.shared.lock() {
            data.profile_on.clear();
            data.profile_off.clear();
            data.close_requested = false;
        }
    }

    pub fn has_line(&self) -> bool {
        self.start.is_some() && self.end.is_some()
    }

    pub fn set_line(&mut self, start: (u16, u16), end: (u16, u16), frame: &PreviewFrame) {
        self.start = Some(start);
        self.end = Some(end);
        self.recompute(frame);
    }

    pub fn open_window(&mut self) {
        self.window_open = true;
        if let Ok(mut data) = self.shared.lock() {
            data.close_requested = false;
        }
    }

    pub fn recompute(&mut self, frame: &PreviewFrame) {
        let (Some(start), Some(end)) = (self.start, self.end) else {
            self.profile_on.clear();
            self.profile_off.clear();
            return;
        };

        self.profile_on.clear();
        self.profile_off.clear();
        let width = usize::from(frame.width.max(1));
        for (x, y) in bresenham_line(start, end) {
            let idx = usize::from(y) * width + usize::from(x);
            self.profile_on.push(frame.pixels_on[idx] as f32);
            self.profile_off.push(frame.pixels_off[idx] as f32);
        }
    }

    pub fn show_window(&mut self, ctx: &egui::Context) {
        {
            let mut data = self.shared.lock().unwrap();
            data.profile_on.clone_from(&self.profile_on);
            data.profile_off.clone_from(&self.profile_off);
        }

        if !self.window_open {
            if let Ok(mut data) = self.shared.lock() {
                data.close_requested = false;
            }
            return;
        }

        let shared = Arc::clone(&self.shared);
        let viewport_id = egui::ViewportId::from_hash_of("viewer_line_profile_window");
        let viewport_visuals = ctx.style().visuals.clone();
        ctx.show_viewport_deferred(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(LINE_PROFILE_VIEWPORT_TITLE)
                .with_inner_size([560.0, 360.0]),
            move |ctx, class| {
                ctx.set_visuals(viewport_visuals.clone());
                match class {
                    egui::viewport::ViewportClass::Deferred => {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            render_line_profile_viewport(ui, &shared);
                        });
                        if ctx.input(|input| input.viewport().close_requested()) {
                            if let Ok(mut data) = shared.lock() {
                                data.close_requested = true;
                            }
                        }
                    }
                    egui::viewport::ViewportClass::Embedded => {
                        let mut open = true;
                        egui::Window::new(LINE_PROFILE_VIEWPORT_TITLE)
                            .open(&mut open)
                            .default_size([560.0, 360.0])
                            .show(ctx, |ui| {
                                render_line_profile_viewport(ui, &shared);
                            });
                        if !open {
                            if let Ok(mut data) = shared.lock() {
                                data.close_requested = true;
                            }
                        }
                    }
                    _ => {}
                }
            },
        );

        let close_requested = {
            let mut data = self.shared.lock().unwrap();
            let close = data.close_requested;
            data.close_requested = false;
            close
        };
        self.window_open = !close_requested;
    }
}

fn render_line_profile_viewport(ui: &mut egui::Ui, shared: &Arc<Mutex<LineProfileViewportData>>) {
    let mut data = shared.lock().unwrap();
    if data.profile_on.is_empty() {
        ui.weak("Draw a line on the preview to inspect the ON/OFF profile.");
        return;
    }

    let on_points: Vec<[f64; 2]> = data
        .profile_on
        .iter()
        .enumerate()
        .map(|(index, value)| [index as f64, f64::from(*value)])
        .collect();
    let off_points: Vec<[f64; 2]> = data
        .profile_off
        .iter()
        .enumerate()
        .map(|(index, value)| [index as f64, f64::from(*value)])
        .collect();
    let show_sum = data.show_sum;
    let sum_points: Vec<[f64; 2]> = data
        .profile_on
        .iter()
        .zip(&data.profile_off)
        .enumerate()
        .map(|(index, (on, off))| [index as f64, f64::from(*on + *off)])
        .collect();
    Plot::new("line_profile_plot")
        .allow_boxed_zoom(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .x_axis_label("Position (px)")
        .y_axis_label("Intensity")
        .legend(Legend::default())
        .height(260.0)
        .show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(on_points)
                    .name("ON")
                    .color(Color32::from_rgb(0, 220, 120)),
            );
            plot_ui.line(
                Line::new(off_points)
                    .name("OFF")
                    .color(Color32::from_rgb(255, 96, 96)),
            );
            if show_sum {
                plot_ui.line(
                    Line::new(sum_points)
                        .name("Sum")
                        .color(Color32::from_rgb(140, 180, 255)),
                );
            }
        });
    ui.checkbox(&mut data.show_sum, "Show ON+OFF sum");
}

pub fn bresenham_line(start: (u16, u16), end: (u16, u16)) -> Vec<(u16, u16)> {
    let (mut x0, mut y0) = (i32::from(start.0), i32::from(start.1));
    let (x1, y1) = (i32::from(end.0), i32::from(end.1));
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    let mut points = Vec::new();

    loop {
        points.push((x0.max(0) as u16, y0.max(0) as u16));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let double_error = error * 2;
        if double_error >= dy {
            error += dy;
            x0 += sx;
        }
        if double_error <= dx {
            error += dx;
            y0 += sy;
        }
    }

    points
}

#[cfg(test)]
mod tests {
    use super::bresenham_line;

    #[test]
    fn bresenham_covers_both_endpoints() {
        let points = bresenham_line((1, 1), (4, 2));
        assert_eq!(points.first().copied(), Some((1, 1)));
        assert_eq!(points.last().copied(), Some((4, 2)));
    }
}
