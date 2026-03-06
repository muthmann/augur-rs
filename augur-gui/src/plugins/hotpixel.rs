use augur_core::{
    analysis::{
        hotpixel::{HotpixelConfig, HotpixelDetector},
        AnalysisOutput, Analyzer,
    },
    config::CameraConfig,
    pipeline::PreviewFrame,
};

use crate::plugin::AnalysisPlugin;

pub struct HotpixelPlugin {
    config: HotpixelConfig,
    detector: HotpixelDetector,
}

impl Default for HotpixelPlugin {
    fn default() -> Self {
        let config = HotpixelConfig::default();
        Self {
            detector: HotpixelDetector::new(config.clone()),
            config,
        }
    }
}

impl HotpixelPlugin {
    fn rebuild_detector(&mut self) {
        self.detector = HotpixelDetector::new(self.config.clone());
    }
}

impl AnalysisPlugin for HotpixelPlugin {
    fn name(&self) -> &str {
        "Hotpixel Detection"
    }

    fn description(&self) -> &str {
        "GUI-only analysis. Identifies pixels that fire at abnormally high rates regardless of scene activity."
    }

    fn enabled(&self) -> bool {
        self.config.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.config.enabled != enabled {
            self.config.enabled = enabled;
            self.rebuild_detector();
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui, _config: &mut CameraConfig) -> bool {
        let mut changed = false;

        egui::CollapsingHeader::new(self.name())
            .default_open(true)
            .show(ui, |ui| {
                ui.weak(self.description());
                changed |= ui
                    .add(egui::Slider::new(&mut self.config.history_depth, 4..=64).text(
                        "Smoothing depth",
                    ))
                    .on_hover_text("Number of frames used for the exponential moving average. Higher = more stable detection but slower to react to changes.")
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.config.threshold_factor, 2.0..=50.0)
                            .text("Threshold factor"),
                    )
                    .on_hover_text("A pixel is flagged as hot if its event count exceeds this multiple of the global mean. Lower = more aggressive detection.")
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.config.min_absolute_count, 1..=100)
                            .text("Min absolute count"),
                    )
                    .on_hover_text("Minimum event count per frame for a pixel to be considered hot.")
                    .changed();
            });

        if changed {
            self.rebuild_detector();
        }

        false
    }

    fn process_frame(&mut self, frame: &PreviewFrame, output: &mut AnalysisOutput) {
        let plugin_output = self.detector.process_frame(frame);
        output.overlays.extend(plugin_output.overlays);
        output.warnings.extend(plugin_output.warnings);
    }

    fn reset(&mut self) {
        self.detector.reset();
    }
}
