use super::{AnalysisOutput, AnalysisSeverity, AnalysisWarning, Analyzer, Overlay, Pixel};
use crate::pipeline::PreviewFrame;

const HOTPIXEL_OVERLAY_COLOR: [u8; 4] = [255, 32, 32, 160];

#[derive(Debug, Clone, PartialEq)]
pub struct HotpixelConfig {
    pub enabled: bool,
    pub history_depth: u32,
    pub threshold_factor: f32,
    pub min_absolute_count: u16,
}

impl Default for HotpixelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            history_depth: 16,
            threshold_factor: 10.0,
            min_absolute_count: 5,
        }
    }
}

impl HotpixelConfig {
    fn alpha(&self) -> f32 {
        1.0 / self.history_depth.max(1) as f32
    }
}

pub struct HotpixelDetector {
    config: HotpixelConfig,
    ema_counts: Vec<f32>,
    frame_dims: Option<(u16, u16)>,
}

impl HotpixelDetector {
    pub fn new(config: HotpixelConfig) -> Self {
        Self {
            config,
            ema_counts: Vec::new(),
            frame_dims: None,
        }
    }

    fn ensure_state(&mut self, frame: &PreviewFrame) {
        let dims = (frame.width, frame.height);
        if self.frame_dims != Some(dims) || self.ema_counts.len() != frame.pixels.len() {
            self.ema_counts = vec![0.0; frame.pixels.len()];
            self.frame_dims = Some(dims);
        }
    }
}

impl Analyzer for HotpixelDetector {
    fn name(&self) -> &str {
        "Hotpixel Detector"
    }

    fn process_frame(&mut self, frame: &PreviewFrame) -> AnalysisOutput {
        if !self.config.enabled || frame.pixels.is_empty() {
            return AnalysisOutput::default();
        }

        self.ensure_state(frame);

        let alpha = self.config.alpha();
        let decay = 1.0 - alpha;
        let mut ema_sum = 0.0;

        for (ema, &pixel_count) in self.ema_counts.iter_mut().zip(&frame.pixels) {
            *ema = alpha * f32::from(pixel_count) + decay * *ema;
            ema_sum += *ema;
        }

        let global_mean = ema_sum / self.ema_counts.len() as f32;
        let threshold = (global_mean * self.config.threshold_factor)
            .max(f32::from(self.config.min_absolute_count));
        let mut hot_pixels = Vec::new();

        for (idx, &ema) in self.ema_counts.iter().enumerate() {
            if ema < threshold {
                continue;
            }

            let x = (idx % frame.width as usize) as u16;
            let y = (idx / frame.width as usize) as u16;
            hot_pixels.push(Pixel::new(x, y));
        }

        if hot_pixels.is_empty() {
            return AnalysisOutput::default();
        }

        let severity = if hot_pixels.len() > 20 {
            AnalysisSeverity::Error
        } else if hot_pixels.len() > 5 {
            AnalysisSeverity::Warning
        } else {
            AnalysisSeverity::Info
        };

        let count = hot_pixels.len();
        AnalysisOutput {
            overlays: vec![Overlay::HighlightPixels {
                pixels: hot_pixels,
                color: HOTPIXEL_OVERLAY_COLOR,
            }],
            warnings: vec![AnalysisWarning {
                source: self.name().to_owned(),
                severity,
                message: format!("{count} suspected hot pixels detected"),
            }],
        }
    }

    fn reset(&mut self) {
        self.ema_counts.fill(0.0);
        self.frame_dims = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u16, height: u16, pixels: Vec<u16>) -> PreviewFrame {
        PreviewFrame {
            width,
            height,
            pixels,
            window_start_us: 0,
            window_end_us: 1,
        }
    }

    fn highlighted_pixels(output: &AnalysisOutput) -> Vec<Pixel> {
        output
            .overlays
            .iter()
            .find_map(|overlay| match overlay {
                Overlay::HighlightPixels { pixels, .. } => Some(pixels.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }

    #[test]
    fn detects_consistent_hot_pixel() {
        let mut detector = HotpixelDetector::new(HotpixelConfig {
            history_depth: 4,
            threshold_factor: 5.0,
            min_absolute_count: 5,
            ..HotpixelConfig::default()
        });

        let mut output = AnalysisOutput::default();
        for _ in 0..4 {
            output = detector.process_frame(&frame(4, 4, {
                let mut pixels = vec![0; 16];
                pixels[6] = 40;
                pixels
            }));
        }

        assert_eq!(output.warnings.len(), 1);
        assert_eq!(highlighted_pixels(&output), vec![Pixel::new(2, 1)]);
    }

    #[test]
    fn ignores_uniform_activity() {
        let mut detector = HotpixelDetector::new(HotpixelConfig {
            history_depth: 4,
            threshold_factor: 2.0,
            min_absolute_count: 1,
            ..HotpixelConfig::default()
        });

        let output = detector.process_frame(&frame(4, 4, vec![8; 16]));

        assert!(output.warnings.is_empty());
        assert!(output.overlays.is_empty());
    }

    #[test]
    fn respects_min_absolute_count() {
        let mut detector = HotpixelDetector::new(HotpixelConfig {
            history_depth: 1,
            threshold_factor: 2.0,
            min_absolute_count: 10,
            ..HotpixelConfig::default()
        });

        let output = detector.process_frame(&frame(4, 4, {
            let mut pixels = vec![0; 16];
            pixels[5] = 6;
            pixels
        }));

        assert!(output.warnings.is_empty());
        assert!(output.overlays.is_empty());
    }
}
