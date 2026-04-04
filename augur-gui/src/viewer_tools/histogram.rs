use std::sync::{Arc, Mutex};

use crate::{colormap::Colormap, preview::PreviewMode};
use egui::{Align2, Color32, Sense};
use egui_plot::{Bar, BarChart, LineStyle, MarkerShape, Plot, PlotPoint, Points, Text, VLine};

const HISTOGRAM_VIEWPORT_TITLE: &str = "Histogram & Brightness/Contrast - AugurRS";
const MARKER_HOVER_BINS: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContrastMode {
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContrastSettings {
    pub mode: ContrastMode,
    pub auto_percentile: f32,
    pub display_min: u16,
    pub display_max: u16,
    pub gamma: f32,
}

impl Default for ContrastSettings {
    fn default() -> Self {
        Self {
            mode: ContrastMode::Auto,
            auto_percentile: 99.5,
            display_min: 0,
            display_max: 255,
            gamma: 0.5,
        }
    }
}

impl ContrastSettings {
    pub fn update_auto_range(&mut self, histogram: &[u64]) {
        if self.mode != ContrastMode::Auto {
            return;
        }
        self.display_min = 0;
        self.display_max = percentile_bin(histogram, self.auto_percentile).max(1);
    }

    pub fn set_manual_range(&mut self, display_min: u16, display_max: u16) {
        self.mode = ContrastMode::Manual;
        self.display_min = display_min.min(display_max.saturating_sub(1));
        self.display_max = display_max.max(self.display_min.saturating_add(1));
    }
}

#[derive(Debug)]
pub struct HistogramWindow {
    pub open: bool,
    shared: Arc<Mutex<HistogramViewportData>>,
}

impl Default for HistogramWindow {
    fn default() -> Self {
        Self {
            open: false,
            shared: Arc::new(Mutex::new(HistogramViewportData::default())),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct HistogramViewportData {
    histogram: Arc<Vec<u64>>,
    contrast: ContrastSettings,
    mode: PreviewMode,
    log_scale: bool,
    drag_target: Option<MarkerDragTarget>,
    close_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkerDragTarget {
    Min,
    Max,
}

impl HistogramWindow {
    pub fn set_histogram(&mut self, histogram: Vec<u64>) {
        if let Ok(mut data) = self.shared.lock() {
            data.histogram = Arc::new(histogram);
        }
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        contrast: &mut ContrastSettings,
        mode: PreviewMode,
    ) {
        {
            let mut data = self.shared.lock().unwrap();
            data.contrast = contrast.clone();
            data.mode = mode;
        }

        if !self.open {
            if let Ok(mut data) = self.shared.lock() {
                data.close_requested = false;
                data.drag_target = None;
            }
            return;
        }

        let shared = Arc::clone(&self.shared);
        let viewport_id = egui::ViewportId::from_hash_of("viewer_histogram_window");
        let viewport_visuals = ctx.style().visuals.clone();
        ctx.show_viewport_deferred(
            viewport_id,
            egui::ViewportBuilder::default()
                .with_title(HISTOGRAM_VIEWPORT_TITLE)
                .with_inner_size([560.0, 420.0]),
            move |ctx, class| {
                ctx.set_visuals(viewport_visuals.clone());
                match class {
                    egui::viewport::ViewportClass::Deferred => {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            render_histogram_viewport(ui, &shared);
                        });
                        if ctx.input(|input| input.viewport().close_requested()) {
                            if let Ok(mut data) = shared.lock() {
                                data.close_requested = true;
                            }
                        }
                    }
                    egui::viewport::ViewportClass::Embedded => {
                        let mut open = true;
                        egui::Window::new(HISTOGRAM_VIEWPORT_TITLE)
                            .open(&mut open)
                            .default_size([560.0, 420.0])
                            .show(ctx, |ui| {
                                render_histogram_viewport(ui, &shared);
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

        let (close_requested, updated_contrast) = {
            let mut data = self.shared.lock().unwrap();
            let result = (data.close_requested, data.contrast.clone());
            data.close_requested = false;
            result
        };
        self.open = !close_requested;
        *contrast = updated_contrast;
    }
}

fn render_histogram_viewport(ui: &mut egui::Ui, shared: &Arc<Mutex<HistogramViewportData>>) {
    let mut data = shared.lock().unwrap();
    let histogram = Arc::clone(&data.histogram);
    if histogram.is_empty() {
        ui.weak("No preview frame histogram available yet.");
        return;
    }

    ui.small("Drag the blue/yellow markers to adjust brightness & contrast.");

    let max_bin = histogram.len().saturating_sub(1);
    let plot_max = if data.log_scale {
        histogram
            .iter()
            .copied()
            .max()
            .map(log_value)
            .unwrap_or(1.0)
            .max(1.0)
    } else {
        histogram.iter().copied().max().unwrap_or(1) as f64
    };
    let bars: Vec<Bar> = histogram
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let height = if data.log_scale {
                log_value(*value)
            } else {
                *value as f64
            };
            Bar::new(index as f64, height).width(0.95)
        })
        .collect();
    let y_axis_label = if data.log_scale {
        "Count (log)"
    } else {
        "Count"
    };
    let histogram_for_hover = Arc::clone(&histogram);

    let mut pointer_x = None;
    let plot_response = Plot::new("preview_histogram_plot")
        .allow_boxed_zoom(false)
        .allow_double_click_reset(false)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .include_x(0.0)
        .include_x(max_bin as f64)
        .include_y(0.0)
        .include_y(plot_max)
        .x_axis_label("Pixel Intensity")
        .y_axis_label(y_axis_label)
        .label_formatter(move |_name, value| {
            let intensity = value.x.round().clamp(0.0, max_bin as f64) as usize;
            let count = histogram_for_hover
                .get(intensity)
                .copied()
                .unwrap_or_default();
            format!("Intensity: {intensity}\nCount: {count}")
        })
        .height(220.0)
        .show(ui, |plot_ui| {
            let hovered_marker = marker_hover(
                plot_ui.pointer_coordinate().map(|point| point.x),
                data.contrast.display_min,
                data.contrast.display_max,
            );
            let min_active = hovered_marker == Some(MarkerDragTarget::Min)
                || data.drag_target == Some(MarkerDragTarget::Min);
            let max_active = hovered_marker == Some(MarkerDragTarget::Max)
                || data.drag_target == Some(MarkerDragTarget::Max);
            let handle_y = plot_max * 0.03;
            let label_y = plot_max.max(1.0) * 0.98;

            plot_ui.bar_chart(BarChart::new(bars).color(Color32::from_rgb(200, 200, 220)));
            plot_ui.vline(
                VLine::new(f64::from(data.contrast.display_min))
                    .color(if min_active {
                        Color32::from_rgb(120, 240, 255)
                    } else {
                        Color32::from_rgb(0, 220, 255)
                    })
                    .width(if min_active { 3.5 } else { 2.0 })
                    .style(LineStyle::Solid),
            );
            plot_ui.vline(
                VLine::new(f64::from(data.contrast.display_max))
                    .color(if max_active {
                        Color32::from_rgb(255, 226, 120)
                    } else {
                        Color32::from_rgb(255, 196, 64)
                    })
                    .width(if max_active { 3.5 } else { 2.0 })
                    .style(LineStyle::Solid),
            );
            plot_ui.points(
                Points::new(vec![[f64::from(data.contrast.display_min), handle_y]])
                    .shape(MarkerShape::Down)
                    .radius(if min_active { 8.0 } else { 6.0 })
                    .color(Color32::from_rgb(0, 220, 255)),
            );
            plot_ui.points(
                Points::new(vec![[f64::from(data.contrast.display_max), handle_y]])
                    .shape(MarkerShape::Down)
                    .radius(if max_active { 8.0 } else { 6.0 })
                    .color(Color32::from_rgb(255, 196, 64)),
            );
            plot_ui.text(
                Text::new(
                    PlotPoint::new(f64::from(data.contrast.display_min), label_y),
                    format!("Min {}", data.contrast.display_min),
                )
                .anchor(Align2::LEFT_TOP)
                .color(Color32::from_rgb(0, 220, 255)),
            );
            plot_ui.text(
                Text::new(
                    PlotPoint::new(f64::from(data.contrast.display_max), label_y),
                    format!("Max {}", data.contrast.display_max),
                )
                .anchor(Align2::RIGHT_TOP)
                .color(Color32::from_rgb(255, 196, 64)),
            );
            pointer_x = plot_ui.pointer_coordinate().map(|point| point.x);
        });

    if let Some(pointer_x) = pointer_x {
        let hovered_bin = pointer_x.round().clamp(0.0, max_bin as f64) as u16;
        if plot_response.response.drag_started() {
            data.drag_target = marker_hover(
                Some(pointer_x),
                data.contrast.display_min,
                data.contrast.display_max,
            );
        }
        if plot_response.response.dragged() {
            match data.drag_target {
                Some(MarkerDragTarget::Min) => {
                    let display_max = data.contrast.display_max;
                    data.contrast.set_manual_range(
                        hovered_bin.min(display_max.saturating_sub(1)),
                        display_max,
                    );
                }
                Some(MarkerDragTarget::Max) => {
                    let display_min = data.contrast.display_min;
                    data.contrast.set_manual_range(
                        display_min,
                        hovered_bin.max(display_min.saturating_add(1)),
                    );
                }
                None => {}
            }
        }
    }
    if plot_response.response.drag_stopped() {
        data.drag_target = None;
    }

    ui.horizontal(|ui| {
        ui.small(format!(
            "Display range: {} .. {}",
            data.contrast.display_min, data.contrast.display_max
        ));
        ui.separator();
        ui.small(match data.contrast.mode {
            ContrastMode::Auto => "Mode: Auto",
            ContrastMode::Manual => "Mode: Manual",
        });
    });

    ui.horizontal(|ui| {
        if ui.button("Auto").clicked() {
            data.contrast.mode = ContrastMode::Auto;
            data.contrast.update_auto_range(histogram.as_slice());
        }
        if ui.button("Reset").clicked() {
            data.contrast.gamma = 0.5;
            data.contrast.set_manual_range(0, max_bin.max(1) as u16);
        }
        ui.checkbox(&mut data.log_scale, "Log scale");
    });

    ui.add(
        egui::Slider::new(&mut data.contrast.gamma, 0.1..=2.0)
            .text("Gamma")
            .logarithmic(true),
    );
    if data.contrast.mode == ContrastMode::Auto {
        ui.add(
            egui::Slider::new(&mut data.contrast.auto_percentile, 90.0..=100.0)
                .text("Auto percentile"),
        );
        data.contrast.update_auto_range(histogram.as_slice());
    }

    let gradient_size = egui::vec2(ui.available_width().max(120.0), 12.0);
    let (gradient_rect, _) = ui.allocate_exact_size(gradient_size, Sense::hover());
    let gradient_painter = ui.painter_at(gradient_rect);
    for step in 0..64 {
        let t0 = step as f32 / 64.0;
        let t1 = (step + 1) as f32 / 64.0;
        let x0 = egui::lerp(gradient_rect.left()..=gradient_rect.right(), t0);
        let x1 = egui::lerp(gradient_rect.left()..=gradient_rect.right(), t1);
        gradient_painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x0, gradient_rect.top()),
                egui::pos2(x1, gradient_rect.bottom()),
            ),
            0.0,
            gradient_color(data.mode, (t0 + t1) * 0.5),
        );
    }
    gradient_painter.rect_stroke(
        gradient_rect,
        1.0,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    ui.small(match data.mode {
        PreviewMode::Intensity(colormap) => format!("{} display ramp", colormap.label()),
        mode => mode.ramp_label().to_owned(),
    });

    let mut display_min = data.contrast.display_min;
    let mut display_max = data.contrast.display_max.max(display_min.saturating_add(1));
    ui.horizontal(|ui| {
        ui.add(egui::Slider::new(&mut display_min, 0..=max_bin as u16).text("Min"));
        ui.add(egui::Slider::new(&mut display_max, 1..=max_bin.max(1) as u16).text("Max"));
    });
    if display_min != data.contrast.display_min || display_max != data.contrast.display_max {
        data.contrast.set_manual_range(display_min, display_max);
    }
}

fn marker_hover(
    pointer_x: Option<f64>,
    display_min: u16,
    display_max: u16,
) -> Option<MarkerDragTarget> {
    let pointer_x = pointer_x?;
    let min_delta = (pointer_x - f64::from(display_min)).abs();
    let max_delta = (pointer_x - f64::from(display_max)).abs();
    if min_delta.min(max_delta) > MARKER_HOVER_BINS {
        return None;
    }
    Some(nearest_marker(
        pointer_x.round() as u16,
        display_min,
        display_max,
    ))
}

fn nearest_marker(value: u16, display_min: u16, display_max: u16) -> MarkerDragTarget {
    let min_delta = value.abs_diff(display_min);
    let max_delta = value.abs_diff(display_max);
    if min_delta <= max_delta {
        MarkerDragTarget::Min
    } else {
        MarkerDragTarget::Max
    }
}

fn gradient_color(mode: PreviewMode, value: f32) -> Color32 {
    match mode {
        PreviewMode::RedBlue => {
            if value < 0.5 {
                let channel = ((value * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8;
                Color32::from_rgb(0, 0, channel)
            } else {
                let channel = (((value - 0.5) * 2.0).clamp(0.0, 1.0) * 255.0).round() as u8;
                Color32::from_rgb(channel, 0, channel)
            }
        }
        PreviewMode::SignedCount => Colormap::BlueWhiteRed.lookup(value),
        PreviewMode::Intensity(colormap) => colormap.lookup(value),
        PreviewMode::TimeSurface => {
            let channel = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color32::from_gray(channel)
        }
    }
}

fn percentile_bin(histogram: &[u64], percentile: f32) -> u16 {
    if histogram.is_empty() {
        return 1;
    }

    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return 1;
    }

    let target =
        ((total as f64 * percentile.clamp(0.0, 100.0) as f64 / 100.0).ceil() as u64).max(1);
    let mut cumulative = 0u64;
    for (bin, count) in histogram.iter().copied().enumerate() {
        cumulative += count;
        if cumulative >= target {
            return bin.min(u16::MAX as usize) as u16;
        }
    }
    histogram.len().saturating_sub(1).min(u16::MAX as usize) as u16
}

fn log_value(value: u64) -> f64 {
    if value == 0 {
        0.0
    } else {
        (value as f64 + 1.0).log10()
    }
}

#[cfg(test)]
mod tests {
    use super::{ContrastMode, ContrastSettings};

    #[test]
    fn auto_range_picks_percentile_bin() {
        let mut settings = ContrastSettings::default();
        settings.update_auto_range(&[1, 1, 10]);
        assert_eq!(settings.display_min, 0);
        assert!(settings.display_max >= 1);
        assert_eq!(settings.mode, ContrastMode::Auto);
    }

    #[test]
    fn manual_range_keeps_max_above_min() {
        let mut settings = ContrastSettings::default();
        settings.set_manual_range(10, 10);
        assert_eq!(settings.display_min, 9);
        assert_eq!(settings.display_max, 10);
        assert_eq!(settings.mode, ContrastMode::Manual);
    }
}
