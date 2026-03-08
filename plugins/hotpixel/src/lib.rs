use augur_core::{
    analysis::{
        hotpixel::{HotpixelConfig, HotpixelDetector},
        AnalysisSeverity as CoreSeverity, Analyzer, Overlay,
    },
    pipeline::PreviewFrame,
};
use augur_plugin_api::{
    export_plugin, AnalysisSeverity, FfiPixel, HostContext, HostOutput, Plugin, PluginFrame,
    SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry,
};
use serde_json::{json, Value};

struct HotpixelPlugin {
    config: HotpixelConfig,
    detector: HotpixelDetector,
    last_detected_count: usize,
}

impl Default for HotpixelPlugin {
    fn default() -> Self {
        let config = HotpixelConfig::default();
        Self {
            detector: HotpixelDetector::new(config.clone()),
            config,
            last_detected_count: 0,
        }
    }
}

impl HotpixelPlugin {
    fn rebuild_detector(&mut self) {
        self.detector = HotpixelDetector::new(self.config.clone());
    }

    fn build_preview_frame(frame: &PluginFrame<'_>) -> PreviewFrame {
        let n = frame.width() as usize * frame.height() as usize;
        PreviewFrame {
            width: frame.width(),
            height: frame.height(),
            pixels: frame.pixels().to_vec(),
            pixels_on: vec![0; n],
            pixels_off: vec![0; n],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: frame.window_start_us(),
            window_end_us: frame.window_end_us(),
        }
    }

    fn severity(severity: CoreSeverity) -> AnalysisSeverity {
        match severity {
            CoreSeverity::Info => AnalysisSeverity::Info,
            CoreSeverity::Warning => AnalysisSeverity::Warning,
            CoreSeverity::Error => AnalysisSeverity::Error,
        }
    }

    fn parse_usize(value: Value) -> Option<usize> {
        value.as_u64().and_then(|value| usize::try_from(value).ok())
    }
}

impl Plugin for HotpixelPlugin {
    fn name(&self) -> &'static str {
        "Hotpixel Detection"
    }

    fn description(&self) -> &'static str {
        "Runtime-loaded analysis. Identifies pixels that fire at abnormally high rates regardless of scene activity."
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

    fn reset(&mut self) {
        self.last_detected_count = 0;
        self.detector.reset();
    }

    fn process_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        _context: &mut HostContext<'_>,
    ) {
        let preview = Self::build_preview_frame(frame);
        let result = self.detector.process_frame(&preview);
        self.last_detected_count = 0;

        for overlay in result.overlays {
            if let Overlay::HighlightPixels { pixels, color } = overlay {
                let pixels: Vec<FfiPixel> = pixels
                    .into_iter()
                    .map(|pixel| FfiPixel {
                        x: pixel.x,
                        y: pixel.y,
                    })
                    .collect();
                self.last_detected_count = pixels.len();
                output.add_highlight_pixels(&pixels, color);
            }
        }

        for warning in result.warnings {
            output.add_warning(
                &warning.source,
                Self::severity(warning.severity),
                &warning.message,
            );
        }
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            sections: vec![SettingsSection {
                label: "Detection".into(),
                description: Some(
                    "Persistent event-rate spikes are tracked with an exponential moving average."
                        .into(),
                ),
                default_open: true,
                items: vec![
                    SettingItem {
                        key: "history_depth".into(),
                        label: "Smoothing depth".into(),
                        tooltip: Some("Higher values react more slowly but suppress frame-to-frame flicker.".into()),
                        kind: SettingKind::I64Slider {
                            min: 4,
                            max: 64,
                            default: i64::from(self.config.history_depth),
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "threshold_factor".into(),
                        label: "Threshold factor".into(),
                        tooltip: Some("A pixel is flagged when its activity exceeds this multiple of the global mean.".into()),
                        kind: SettingKind::F64Slider {
                            min: 2.0,
                            max: 50.0,
                            default: f64::from(self.config.threshold_factor),
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "min_absolute_count".into(),
                        label: "Min absolute count".into(),
                        tooltip: Some("Minimum per-frame event count required before a pixel can be treated as hot.".into()),
                        kind: SettingKind::I64Slider {
                            min: 1,
                            max: 100,
                            default: i64::from(self.config.min_absolute_count),
                            suffix: None,
                        },
                    },
                ],
            }],
        }
    }

    fn get_setting(&self, key: &str) -> Option<Value> {
        match key {
            "history_depth" => Some(json!(self.config.history_depth)),
            "threshold_factor" => Some(json!(self.config.threshold_factor)),
            "min_absolute_count" => Some(json!(self.config.min_absolute_count)),
            _ => None,
        }
    }

    fn set_setting(&mut self, key: &str, value: Value) -> Result<(), String> {
        match key {
            "history_depth" => {
                let Some(value) = Self::parse_usize(value) else {
                    return Err("history_depth must be an integer".into());
                };
                self.config.history_depth =
                    u32::try_from(value.clamp(4, 64)).expect("clamped history depth fits in u32");
            }
            "threshold_factor" => {
                let Some(value) = value.as_f64() else {
                    return Err("threshold_factor must be numeric".into());
                };
                self.config.threshold_factor = value.clamp(2.0, 50.0) as f32;
            }
            "min_absolute_count" => {
                let Some(value) = Self::parse_usize(value) else {
                    return Err("min_absolute_count must be an integer".into());
                };
                self.config.min_absolute_count = u16::try_from(value.clamp(1, 100))
                    .expect("clamped min absolute count fits in u16");
            }
            _ => return Err(format!("unknown setting: {key}")),
        }

        self.rebuild_detector();
        Ok(())
    }

    fn status_entries(&self) -> Vec<StatusEntry> {
        vec![StatusEntry::Text(format!(
            "Last frame: {} detected hotpixels.",
            self.last_detected_count
        ))]
    }
}

export_plugin!(HotpixelPlugin);
