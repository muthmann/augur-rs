use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, AnalysisWarning, Overlay, Pixel},
    pipeline::PreviewFrame,
};

const HOTPIXEL_OVERLAY_COLOR: [u8; 4] = [255, 32, 32, 160];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltInHotpixelDetection {
    enabled: bool,
    history_depth: u32,
    threshold_factor: f32,
    min_absolute_count: u16,
    ema_counts: Vec<f32>,
    frame_dims: Option<(u16, u16)>,
    last_detected_pixels: Vec<(u16, u16)>,
    last_detected_count: usize,
    ui_dirty: bool,
}

impl Default for BuiltInHotpixelDetection {
    fn default() -> Self {
        Self {
            enabled: true,
            history_depth: 16,
            threshold_factor: 10.0,
            min_absolute_count: 5,
            ema_counts: Vec::new(),
            frame_dims: None,
            last_detected_pixels: Vec::new(),
            last_detected_count: 0,
            ui_dirty: false,
        }
    }
}

impl BuiltInHotpixelDetection {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.reset();
            self.ui_dirty = true;
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.ui_dirty
    }

    pub(crate) fn detected_pixels(&self) -> &[(u16, u16)] {
        &self.last_detected_pixels
    }

    pub(crate) fn render_ui(&mut self, ui: &mut egui::Ui, show_header: bool) -> bool {
        let mut changed = false;
        let body = |ui: &mut egui::Ui, changed: &mut bool, this: &mut Self| {
            ui.weak(
                "Tracks persistent event-rate spikes and highlights suspected hot pixels on the preview.",
            );

            *changed |= ui
                .add(
                    egui::Slider::new(&mut this.history_depth, 4..=64)
                        .text("Smoothing depth")
                        .clamp_to_range(true),
                )
                .on_hover_text(
                    "Higher values react more slowly but suppress frame-to-frame flicker.",
                )
                .changed();

            *changed |= ui
                .add(
                    egui::Slider::new(&mut this.threshold_factor, 2.0..=50.0)
                        .text("Threshold factor")
                        .clamp_to_range(true),
                )
                .on_hover_text(
                    "A pixel is flagged when its activity exceeds this multiple of the global mean.",
                )
                .changed();

            *changed |= ui
                .add(
                    egui::Slider::new(&mut this.min_absolute_count, 1..=100)
                        .text("Min absolute count")
                        .clamp_to_range(true),
                )
                .on_hover_text(
                    "Minimum per-frame event count required before a pixel can be treated as hot.",
                )
                .changed();

            ui.separator();
            ui.label(format!(
                "Last frame: {} detected hotpixels.",
                this.last_detected_count
            ));
        };

        if show_header {
            egui::CollapsingHeader::new("Hotpixel Detection")
                .default_open(true)
                .show(ui, |ui| body(ui, &mut changed, self));
        } else {
            body(ui, &mut changed, self);
        }
        self.ui_dirty |= changed;
        changed
    }

    pub(crate) fn process_frame(&mut self, frame: &PreviewFrame, output: &mut AnalysisOutput) {
        self.ui_dirty = false;
        self.last_detected_pixels.clear();
        self.last_detected_count = 0;

        if !self.enabled || frame.pixels.is_empty() {
            return;
        }

        self.ensure_state(frame);

        let alpha = 1.0 / self.history_depth.max(1) as f32;
        let decay = 1.0 - alpha;
        let mut ema_sum = 0.0;

        for (ema, &pixel_count) in self.ema_counts.iter_mut().zip(&frame.pixels) {
            *ema = alpha * f32::from(pixel_count) + decay * *ema;
            ema_sum += *ema;
        }

        let global_mean = ema_sum / self.ema_counts.len() as f32;
        let threshold =
            (global_mean * self.threshold_factor).max(f32::from(self.min_absolute_count));
        let mut hot_pixels = Vec::new();

        for (idx, &ema) in self.ema_counts.iter().enumerate() {
            if ema < threshold {
                continue;
            }

            let x = (idx % frame.width as usize) as u16;
            let y = (idx / frame.width as usize) as u16;
            hot_pixels.push(Pixel::new(x, y));
            self.last_detected_pixels.push((x, y));
        }

        if hot_pixels.is_empty() {
            return;
        }

        self.last_detected_count = hot_pixels.len();
        let severity = if hot_pixels.len() > 20 {
            AnalysisSeverity::Error
        } else if hot_pixels.len() > 5 {
            AnalysisSeverity::Warning
        } else {
            AnalysisSeverity::Info
        };

        output.overlays.push(Overlay::HighlightPixels {
            pixels: hot_pixels,
            color: HOTPIXEL_OVERLAY_COLOR,
        });
        output.warnings.push(AnalysisWarning {
            source: "Hotpixel Detection".into(),
            severity,
            message: format!("{} suspected hot pixels detected", self.last_detected_count),
        });
    }

    pub(crate) fn reset(&mut self) {
        self.ema_counts.fill(0.0);
        self.frame_dims = None;
        self.last_detected_pixels.clear();
        self.last_detected_count = 0;
        self.ui_dirty = false;
    }

    fn ensure_state(&mut self, frame: &PreviewFrame) {
        let dims = (frame.width, frame.height);
        if self.frame_dims != Some(dims) || self.ema_counts.len() != frame.pixels.len() {
            self.ema_counts = vec![0.0; frame.pixels.len()];
            self.frame_dims = Some(dims);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u16, height: u16, pixels: Vec<u16>) -> PreviewFrame {
        let n = pixels.len();
        PreviewFrame {
            width,
            height,
            pixels,
            pixels_on: vec![0; n],
            pixels_off: vec![0; n],
            cached_total_histogram: Vec::new(),
            cached_signed_histogram: Vec::new(),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            window_start_us: 0,
            window_end_us: 1,
        }
    }

    #[test]
    fn hotpixel_detection_detects_consistent_hot_pixels() {
        let mut detection = BuiltInHotpixelDetection {
            history_depth: 4,
            threshold_factor: 5.0,
            min_absolute_count: 5,
            ..BuiltInHotpixelDetection::default()
        };

        let mut output = AnalysisOutput::default();
        for _ in 0..4 {
            output = AnalysisOutput::default();
            detection.process_frame(
                &frame(4, 4, {
                    let mut pixels = vec![0; 16];
                    pixels[6] = 40;
                    pixels
                }),
                &mut output,
            );
        }

        assert_eq!(detection.detected_pixels(), &[(2, 1)]);
        assert_eq!(detection.last_detected_count, 1);
        assert_eq!(output.warnings.len(), 1);
        assert!(matches!(
            output.overlays.as_slice(),
            [Overlay::HighlightPixels { .. }]
        ));
    }

    #[test]
    fn hotpixel_detection_ignores_uniform_activity() {
        let mut detection = BuiltInHotpixelDetection {
            history_depth: 4,
            threshold_factor: 2.0,
            min_absolute_count: 1,
            ..BuiltInHotpixelDetection::default()
        };
        let mut output = AnalysisOutput::default();

        detection.process_frame(&frame(4, 4, vec![8; 16]), &mut output);

        assert!(output.warnings.is_empty());
        assert!(output.overlays.is_empty());
        assert!(detection.detected_pixels().is_empty());
    }

    #[test]
    fn disabling_hotpixel_detection_clears_previous_results() {
        let mut detection = BuiltInHotpixelDetection::default();
        let mut output = AnalysisOutput::default();

        detection.process_frame(&frame(2, 1, vec![50, 0]), &mut output);
        detection.set_enabled(false);

        assert!(detection.detected_pixels().is_empty());
        assert_eq!(detection.last_detected_count, 0);
    }
}
