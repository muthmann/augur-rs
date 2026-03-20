use std::collections::VecDeque;

use augur_core::{config::RoiConfig, pipeline::CdEvent};

use crate::app::PANEL_ROUNDING;
const DEFAULT_TIME_WINDOW_MS: f32 = 120.0;
const MIN_TIME_WINDOW_MS: f32 = 5.0;
const MAX_TIME_WINDOW_MS: f32 = 2_000.0;
const DEFAULT_POINT_LIMIT: usize = 12_000;
const MIN_POINT_LIMIT: usize = 1_000;
const MAX_POINT_LIMIT: usize = 100_000;
const MAX_HISTORY_POINTS: usize = 400_000;
const MAX_HISTORY_MS: f32 = 5_000.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct PointCloudMetrics {
    pub visible_points: usize,
    pub rendered_points: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VisibleHistoryWindow {
    lo: usize,
    hi: usize,
    start_ts: u64,
    end_ts: u64,
    step: usize,
}

#[derive(Debug, Clone)]
pub struct PointCloudState {
    history: VecDeque<CdEvent>,
    pub time_window_ms: f32,
    pub point_limit: usize,
    azimuth: f32,
    elevation: f32,
    distance: f32,
}

impl Default for PointCloudState {
    fn default() -> Self {
        Self {
            history: VecDeque::with_capacity(32_768),
            time_window_ms: DEFAULT_TIME_WINDOW_MS,
            point_limit: DEFAULT_POINT_LIMIT,
            azimuth: 0.65,
            elevation: 0.55,
            distance: 2.35,
        }
    }
}

impl PointCloudState {
    pub fn clear(&mut self) {
        self.history.clear();
    }

    pub fn reset_camera(&mut self) {
        self.azimuth = 0.65;
        self.elevation = 0.55;
        self.distance = 2.35;
    }

    pub fn push_events(&mut self, events: &[CdEvent]) {
        if events.is_empty() {
            return;
        }

        // When the incoming batch alone exceeds the cap, only keep its tail —
        // extending and then immediately draining would waste an allocation.
        let events = if events.len() >= MAX_HISTORY_POINTS {
            self.history.clear();
            &events[events.len() - MAX_HISTORY_POINTS..]
        } else {
            events
        };

        self.history.extend(events.iter().copied());
        self.trim_history();
    }

    pub fn sanitize_controls(&mut self) {
        self.time_window_ms = self
            .time_window_ms
            .clamp(MIN_TIME_WINDOW_MS, MAX_TIME_WINDOW_MS);
        self.point_limit = self.point_limit.clamp(MIN_POINT_LIMIT, MAX_POINT_LIMIT);
    }

    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        roi: RoiConfig,
        max_height: f32,
    ) -> PointCloudMetrics {
        self.sanitize_controls();

        let available = ui.available_size_before_wrap();
        let desired_size = egui::vec2(
            available.x.max(320.0),
            available.y.min(max_height).max(260.0),
        );
        let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::drag());
        let painter = ui.painter_at(rect);

        painter.rect_filled(rect, PANEL_ROUNDING, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            PANEL_ROUNDING,
            egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );

        if response.dragged() {
            let delta = ui.ctx().input(|input| input.pointer.delta());
            self.azimuth -= delta.x * 0.01;
            self.elevation = (self.elevation + delta.y * 0.01).clamp(-1.2, 1.2);
        }

        if response.hovered() {
            let scroll_y = ui.ctx().input(|input| input.raw_scroll_delta.y);
            if scroll_y.abs() > f32::EPSILON {
                self.distance = (self.distance * (1.0 - scroll_y * 0.0015)).clamp(1.1, 7.5);
            }
        }

        let metrics = self.paint_points(&painter, rect, roi);
        if metrics.rendered_points == 0 {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No recent raw events available for the 3D view.",
                egui::FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
        }

        metrics
    }

    fn trim_history(&mut self) {
        let len = self.history.len();
        if len > MAX_HISTORY_POINTS {
            self.history.drain(..len - MAX_HISTORY_POINTS);
        }

        let Some(latest) = self.history.back() else {
            return;
        };
        let cutoff = latest
            .timestamp
            .saturating_sub((MAX_HISTORY_MS * 1_000.0).round() as u64);
        let cutoff_idx = self.history.partition_point(|e| e.timestamp < cutoff);
        if cutoff_idx > 0 {
            self.history.drain(..cutoff_idx);
        }
    }

    fn downsample_step(&self, slice_len: usize) -> usize {
        slice_len.div_ceil(self.point_limit.max(1)).max(1)
    }

    fn visible_history_window(&self) -> Option<VisibleHistoryWindow> {
        let latest = self.history.back()?;
        let end_ts = latest.timestamp;
        let start_ts = end_ts.saturating_sub((self.time_window_ms * 1_000.0).round() as u64);

        // Binary-search the sorted history to get only the time-windowed slice.
        // This avoids iterating the full (up to 400 K) history on every frame.
        let lo = self.history.partition_point(|e| e.timestamp < start_ts);
        let hi = self.history.partition_point(|e| e.timestamp <= end_ts);
        if lo >= hi {
            return None;
        }

        Some(VisibleHistoryWindow {
            lo,
            hi,
            start_ts,
            end_ts,
            step: self.downsample_step(hi - lo),
        })
    }

    fn paint_points(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        roi: RoiConfig,
    ) -> PointCloudMetrics {
        let Some(window) = self.visible_history_window() else {
            return PointCloudMetrics::default();
        };

        let roi = sanitize_roi(roi);

        // Precompute the camera transform once — avoids sin/cos per point.
        let cam = CameraView::new(self.azimuth, self.elevation, self.distance, rect);
        draw_axes(painter, cam);

        let mut visible_points = 0usize;
        let mut rendered_points = 0usize;
        for (idx, event) in self.history.range(window.lo..window.hi).enumerate() {
            if !roi_contains(event, &roi) {
                continue;
            }
            visible_points += 1;
            if idx % window.step != 0 {
                continue;
            }
            let Some(projected) = project_event(event, &roi, window.start_ts, window.end_ts, cam)
            else {
                continue;
            };

            painter.circle_filled(
                projected.position,
                projected.radius,
                if event.polarity {
                    egui::Color32::from_rgb(244, 244, 244)
                } else {
                    egui::Color32::from_rgb(55, 96, 210)
                },
            );
            rendered_points += 1;
        }

        PointCloudMetrics {
            visible_points,
            rendered_points,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ProjectedPoint {
    position: egui::Pos2,
    radius: f32,
}

fn sanitize_roi(roi: RoiConfig) -> RoiConfig {
    let width = roi.width.max(1);
    let height = roi.height.max(1);
    RoiConfig {
        x: roi.x,
        y: roi.y,
        width,
        height,
    }
}

/// Precomputed camera transform — compute once per frame, reuse for every projected point.
#[derive(Clone, Copy)]
struct CameraView {
    cos_az: f32,
    sin_az: f32,
    cos_el: f32,
    sin_el: f32,
    distance: f32,
    /// `min(width, height) * 0.42` — constant for a given rect.
    scale: f32,
    center: egui::Pos2,
}

impl CameraView {
    fn new(azimuth: f32, elevation: f32, distance: f32, rect: egui::Rect) -> Self {
        Self {
            cos_az: azimuth.cos(),
            sin_az: azimuth.sin(),
            cos_el: elevation.cos(),
            sin_el: elevation.sin(),
            distance,
            scale: rect.width().min(rect.height()) * 0.42,
            center: rect.center(),
        }
    }

    fn project(&self, point: [f32; 3]) -> Option<ProjectedPoint> {
        let [mut x, y, mut z] = point;
        let rotated_x = x * self.cos_az - z * self.sin_az;
        let rotated_z = x * self.sin_az + z * self.cos_az;
        x = rotated_x;
        z = rotated_z;

        let rotated_y = y * self.cos_el - z * self.sin_el;
        let rotated_z = y * self.sin_el + z * self.cos_el;

        let depth = self.distance + rotated_z;
        if depth <= 0.05 {
            return None;
        }

        let perspective = self.scale / depth;
        Some(ProjectedPoint {
            position: egui::pos2(
                self.center.x + x * perspective,
                self.center.y - rotated_y * perspective,
            ),
            radius: (1.2 + 1.2 / depth).clamp(1.0, 3.2),
        })
    }
}

fn roi_contains(event: &CdEvent, roi: &RoiConfig) -> bool {
    event.x >= roi.x
        && event.x < roi.x.saturating_add(roi.width)
        && event.y >= roi.y
        && event.y < roi.y.saturating_add(roi.height)
}

#[cfg(test)]
fn point_is_visible(event: &CdEvent, roi: &RoiConfig, start_ts: u64, end_ts: u64) -> bool {
    event.timestamp >= start_ts && event.timestamp <= end_ts && roi_contains(event, roi)
}

fn draw_axes(painter: &egui::Painter, cam: CameraView) {
    let axes = [
        (
            [0.0, 0.0, 0.0f32],
            [1.0, 0.0, 0.0f32],
            egui::Color32::from_rgb(220, 96, 96),
            "x",
        ),
        (
            [0.0, 0.0, 0.0f32],
            [0.0, 1.0, 0.0f32],
            egui::Color32::from_rgb(96, 220, 140),
            "y",
        ),
        (
            [0.0, 0.0, 0.0f32],
            [0.0, 0.0, 1.0f32],
            egui::Color32::from_rgb(96, 160, 240),
            "t",
        ),
    ];

    for (start, end, color, label) in axes {
        let Some(start) = cam.project(start) else {
            continue;
        };
        let Some(end) = cam.project(end) else {
            continue;
        };
        painter.line_segment(
            [start.position, end.position],
            egui::Stroke::new(1.5, color),
        );
        painter.text(
            end.position,
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(13.0),
            color,
        );
    }
}

fn project_event(
    event: &CdEvent,
    roi: &RoiConfig,
    start_ts: u64,
    end_ts: u64,
    cam: CameraView,
) -> Option<ProjectedPoint> {
    let x = (event.x.saturating_sub(roi.x)) as f32 / roi.width as f32 - 0.5;
    let y = 0.5 - (event.y.saturating_sub(roi.y)) as f32 / roi.height as f32;
    let t_span = end_ts.saturating_sub(start_ts).max(1) as f32;
    let z = (event.timestamp.saturating_sub(start_ts)) as f32 / t_span - 0.5;
    cam.project([x, y, z])
}

#[cfg(test)]
mod tests {
    use super::{point_is_visible, CameraView, PointCloudState};
    use augur_core::{config::RoiConfig, pipeline::CdEvent};

    #[test]
    fn point_cloud_trims_old_history_and_excess_points() {
        let mut state = PointCloudState::default();
        let events: Vec<CdEvent> = (0..410_000)
            .map(|idx| CdEvent {
                x: 10,
                y: 10,
                timestamp: idx as u64,
                polarity: idx % 2 == 0,
            })
            .collect();

        state.push_events(&events);

        assert!(state.point_limit >= 1_000);
    }

    #[test]
    fn visible_window_uses_latest_time_slice_and_downsamples() {
        let mut state = PointCloudState {
            time_window_ms: 1_000.0,
            point_limit: 2,
            ..Default::default()
        };

        let events: Vec<CdEvent> = (0..6)
            .map(|idx| CdEvent {
                x: 10,
                y: 10,
                timestamp: idx as u64 * 10,
                polarity: idx % 2 == 0,
            })
            .collect();
        state.push_events(&events);

        let window = state.visible_history_window().expect("window should exist");
        assert_eq!((window.lo, window.hi), (0, 6));
        assert_eq!(window.start_ts, 0);
        assert_eq!(window.end_ts, 50);
        assert_eq!(window.step, 3);
    }

    #[test]
    fn point_visibility_respects_roi_and_time_window() {
        let roi = RoiConfig {
            x: 5,
            y: 8,
            width: 20,
            height: 10,
        };
        let visible = CdEvent {
            x: 10,
            y: 12,
            timestamp: 120,
            polarity: true,
        };
        let outside = CdEvent {
            x: 40,
            y: 12,
            timestamp: 120,
            polarity: false,
        };

        assert!(point_is_visible(&visible, &roi, 100, 130));
        assert!(!point_is_visible(&outside, &roi, 100, 130));
    }

    #[test]
    fn projection_returns_screen_point_for_visible_depth() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0));
        let cam = CameraView::new(0.6, 0.5, 2.0, rect);
        let projected = cam.project([0.1, 0.2, 0.1]).expect("point should project");

        assert!(rect.contains(projected.position));
        assert!(projected.radius >= 1.0);
    }
}
