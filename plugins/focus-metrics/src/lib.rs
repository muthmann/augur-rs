use std::collections::VecDeque;

use augur_plugin_api::{
    export_plugin, AnalysisSeverity, HostContext, HostOutput, LocalizationResults, Plugin,
    PluginFrame, PluginInput, SettingItem, SettingKind, SettingsSchema, SettingsSection,
    StatusEntry, CTX_LOCALIZATION_RESULTS,
};
use rustfft::{num_complex::Complex32, FftPlanner};
use serde_json::{json, Value};

const NO_DEPENDENCIES: [&str; 0] = [];
const LOCALIZATION_DEPENDENCY: [&str; 1] = ["Molecule Localization"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusMethod {
    MeanSigma,
    FftHighFrequency,
    AstigmaticRatio,
}

impl FocusMethod {
    fn label(self) -> &'static str {
        match self {
            Self::MeanSigma => "Mean PSF sigma",
            Self::FftHighFrequency => "FFT high frequency",
            Self::AstigmaticRatio => "Astigmatic ratio",
        }
    }

    fn lower_is_better(self) -> bool {
        matches!(self, Self::MeanSigma)
    }

    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::FftHighFrequency,
            2 => Self::AstigmaticRatio,
            _ => Self::MeanSigma,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::MeanSigma => 0,
            Self::FftHighFrequency => 1,
            Self::AstigmaticRatio => 2,
        }
    }
}

#[derive(Debug, Clone)]
struct FocusSettings {
    method: FocusMethod,
    history_depth: usize,
    sigma_min_nm: f64,
    sigma_max_nm: f64,
    nm_per_pixel: f64,
}

impl Default for FocusSettings {
    fn default() -> Self {
        Self {
            method: FocusMethod::MeanSigma,
            history_depth: 120,
            sigma_min_nm: 100.0,
            sigma_max_nm: 190.0,
            nm_per_pixel: 65.0,
        }
    }
}

struct FocusMetricsPlugin {
    enabled: bool,
    settings: FocusSettings,
    history: VecDeque<f64>,
    last_metric: Option<f64>,
    last_status: String,
}

impl Default for FocusMetricsPlugin {
    fn default() -> Self {
        Self {
            enabled: false,
            settings: FocusSettings::default(),
            history: VecDeque::new(),
            last_metric: None,
            last_status: "Enable the plugin to monitor focus quality over time.".into(),
        }
    }
}

impl FocusMetricsPlugin {
    fn clear_history(&mut self) {
        self.history.clear();
        self.last_metric = None;
    }

    fn push_metric(&mut self, value: f64) {
        self.last_metric = Some(value);
        self.history.push_back(value);
        while self.history.len() > self.settings.history_depth {
            self.history.pop_front();
        }
    }

    fn filtered_localizations<'a>(
        &self,
        results: &'a LocalizationResults,
    ) -> Vec<&'a augur_plugin_api::Localization> {
        results
            .localizations
            .iter()
            .filter(|localization| {
                let sigma_x_nm = localization.sigma_x * self.settings.nm_per_pixel;
                let sigma_y_nm = localization.sigma_y * self.settings.nm_per_pixel;
                sigma_x_nm >= self.settings.sigma_min_nm
                    && sigma_x_nm <= self.settings.sigma_max_nm
                    && sigma_y_nm >= self.settings.sigma_min_nm
                    && sigma_y_nm <= self.settings.sigma_max_nm
            })
            .collect()
    }

    fn current_metric_label(&self) -> String {
        match (self.settings.method, self.last_metric) {
            (_, None) => "Current metric: n/a".into(),
            (FocusMethod::MeanSigma, Some(value)) => {
                format!("Current mean sigma: {:.2} nm", value)
            }
            (FocusMethod::FftHighFrequency, Some(value)) => {
                format!("Current FFT metric: {:.3e}", value)
            }
            (FocusMethod::AstigmaticRatio, Some(value)) => {
                format!("Current sigma_x/sigma_y ratio: {:.3}", value)
            }
        }
    }

    fn quality_indicator(&self) -> (&'static str, [u8; 3]) {
        let Some(metric) = self.last_metric else {
            return ("Collecting", [240, 196, 72]);
        };

        match self.settings.method {
            FocusMethod::AstigmaticRatio => {
                let deviation = (metric - 1.0).abs();
                if deviation <= 0.05 {
                    ("Good", [88, 196, 92])
                } else if deviation <= 0.12 {
                    ("Fair", [240, 196, 72])
                } else {
                    ("Poor", [220, 64, 64])
                }
            }
            FocusMethod::MeanSigma | FocusMethod::FftHighFrequency => {
                if self.history.len() < 4 {
                    return ("Collecting", [240, 196, 72]);
                }
                let min = self.history.iter().copied().fold(f64::INFINITY, f64::min);
                let max = self
                    .history
                    .iter()
                    .copied()
                    .fold(f64::NEG_INFINITY, f64::max);
                let span = (max - min).max(1e-9);
                let normalized = match self.settings.method {
                    FocusMethod::MeanSigma => (max - metric) / span,
                    FocusMethod::FftHighFrequency => (metric - min) / span,
                    FocusMethod::AstigmaticRatio => unreachable!(),
                };
                if normalized >= 0.66 {
                    ("Good", [88, 196, 92])
                } else if normalized >= 0.33 {
                    ("Fair", [240, 196, 72])
                } else {
                    ("Poor", [220, 64, 64])
                }
            }
        }
    }

    fn trend_label(&self) -> &'static str {
        if self.history.len() < 6 {
            return "flat";
        }

        let history: Vec<f64> = self.history.iter().copied().collect();
        let split = history.len().saturating_sub(3);
        let previous = &history[..split];
        let current = &history[split..];
        let previous_mean = previous.iter().sum::<f64>() / previous.len() as f64;
        let current_mean = current.iter().sum::<f64>() / current.len() as f64;
        let delta = current_mean - previous_mean;
        let epsilon = previous_mean.abs().max(1.0) * 0.01;

        match self.settings.method {
            FocusMethod::MeanSigma => {
                if delta < -epsilon {
                    "down"
                } else if delta > epsilon {
                    "up"
                } else {
                    "flat"
                }
            }
            FocusMethod::FftHighFrequency => {
                if delta > epsilon {
                    "up"
                } else if delta < -epsilon {
                    "down"
                } else {
                    "flat"
                }
            }
            FocusMethod::AstigmaticRatio => {
                let prev_dev = (previous_mean - 1.0).abs();
                let current_dev = (current_mean - 1.0).abs();
                if current_dev + epsilon < prev_dev {
                    "toward 1.0"
                } else if current_dev > prev_dev + epsilon {
                    "away from 1.0"
                } else {
                    "flat"
                }
            }
        }
    }

    fn process_method(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        context: &mut HostContext<'_>,
    ) {
        match self.settings.method {
            FocusMethod::MeanSigma => {
                let results = match context.get::<LocalizationResults>(CTX_LOCALIZATION_RESULTS) {
                    Ok(Some(results)) => results,
                    Ok(None) => {
                        self.last_status =
                            "Enable the Molecule Localization plugin or switch to FFT mode.".into();
                        output.add_warning(
                            self.name(),
                            AnalysisSeverity::Info,
                            "Mean PSF sigma requires the Molecule Localization plugin.",
                        );
                        return;
                    }
                    Err(err) => {
                        self.last_status = "Localization results could not be decoded.".into();
                        output.add_warning(
                            self.name(),
                            AnalysisSeverity::Error,
                            &format!("Failed to decode localization results: {err}"),
                        );
                        return;
                    }
                };

                let values: Vec<f64> = self
                    .filtered_localizations(&results)
                    .into_iter()
                    .map(|localization| {
                        0.5 * (localization.sigma_x + localization.sigma_y)
                            * self.settings.nm_per_pixel
                    })
                    .collect();
                if values.is_empty() {
                    self.last_status =
                        "No localization fits passed the sigma filter in this frame.".into();
                    return;
                }
                let metric = values.iter().sum::<f64>() / values.len() as f64;
                self.push_metric(metric);
                let window_us = results
                    .frame_window_end_us
                    .saturating_sub(results.frame_window_start_us);
                self.last_status = format!(
                    "Mean PSF sigma from {} localizations over a {} us frame window.",
                    values.len(),
                    window_us
                );
            }
            FocusMethod::FftHighFrequency => {
                let Some(metric) = fft_focus_metric(frame) else {
                    self.last_status =
                        "The preview frame is too sparse for FFT focus estimation.".into();
                    return;
                };
                self.push_metric(metric);
                self.last_status =
                    "Integrated high-frequency power from the preview FFT ring filter.".into();
            }
            FocusMethod::AstigmaticRatio => {
                let results = match context.get::<LocalizationResults>(CTX_LOCALIZATION_RESULTS) {
                    Ok(Some(results)) => results,
                    Ok(None) => {
                        self.last_status =
                            "Enable the Molecule Localization plugin or switch to FFT mode.".into();
                        output.add_warning(
                            self.name(),
                            AnalysisSeverity::Info,
                            "Astigmatic ratio requires the Molecule Localization plugin.",
                        );
                        return;
                    }
                    Err(err) => {
                        self.last_status = "Localization results could not be decoded.".into();
                        output.add_warning(
                            self.name(),
                            AnalysisSeverity::Error,
                            &format!("Failed to decode localization results: {err}"),
                        );
                        return;
                    }
                };

                let values: Vec<f64> = self
                    .filtered_localizations(&results)
                    .into_iter()
                    .filter_map(|localization| {
                        if localization.sigma_y.abs() <= f64::EPSILON {
                            None
                        } else {
                            Some(localization.sigma_x / localization.sigma_y)
                        }
                    })
                    .collect();
                if values.is_empty() {
                    self.last_status =
                        "No localization fits passed the sigma filter in this frame.".into();
                    return;
                }
                let metric = values.iter().sum::<f64>() / values.len() as f64;
                self.push_metric(metric);
                let window_us = results
                    .frame_window_end_us
                    .saturating_sub(results.frame_window_start_us);
                self.last_status = format!(
                    "Astigmatic sigma ratio from {} localizations over a {} us frame window.",
                    values.len(),
                    window_us
                );
            }
        }
    }

    fn parse_usize(value: Value) -> Option<usize> {
        value.as_u64().and_then(|value| usize::try_from(value).ok())
    }
}

impl Plugin for FocusMetricsPlugin {
    fn name(&self) -> &'static str {
        "Focus Metrics"
    }

    fn description(&self) -> &'static str {
        "Running focus metrics derived from molecule fits or from a frequency-domain sharpness estimate."
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.clear_history();
        self.last_status = "Waiting for the next preview frame.".into();
    }

    fn input_kind(&self) -> PluginInput {
        PluginInput::DerivedData
    }

    fn dependencies(&self) -> &[&'static str] {
        match self.settings.method {
            FocusMethod::FftHighFrequency => &NO_DEPENDENCIES,
            FocusMethod::MeanSigma | FocusMethod::AstigmaticRatio => &LOCALIZATION_DEPENDENCY,
        }
    }

    fn process_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        context: &mut HostContext<'_>,
    ) {
        self.process_method(frame, output, context);
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            sections: vec![SettingsSection {
                label: "Focus".into(),
                description: Some(
                    "Focus quality can be inferred from localization statistics or directly from the preview FFT."
                        .into(),
                ),
                default_open: true,
                items: vec![
                    SettingItem {
                        key: "method".into(),
                        label: "Method".into(),
                        tooltip: None,
                        kind: SettingKind::Enum {
                            variants: vec![
                                FocusMethod::MeanSigma.label().into(),
                                FocusMethod::FftHighFrequency.label().into(),
                                FocusMethod::AstigmaticRatio.label().into(),
                            ],
                            default: self.settings.method.index(),
                        },
                    },
                    SettingItem {
                        key: "history_depth".into(),
                        label: "History depth".into(),
                        tooltip: None,
                        kind: SettingKind::I64Slider {
                            min: 16,
                            max: 360,
                            default: i64::try_from(self.settings.history_depth).unwrap_or(120),
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "nm_per_pixel".into(),
                        label: "Scale [nm/px]".into(),
                        tooltip: None,
                        kind: SettingKind::F64Slider {
                            min: 20.0,
                            max: 150.0,
                            default: self.settings.nm_per_pixel,
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "sigma_min_nm".into(),
                        label: "Sigma min [nm]".into(),
                        tooltip: None,
                        kind: SettingKind::F64Drag {
                            min: 10.0,
                            max: 500.0,
                            speed: 1.0,
                            default: self.settings.sigma_min_nm,
                        },
                    },
                    SettingItem {
                        key: "sigma_max_nm".into(),
                        label: "Sigma max [nm]".into(),
                        tooltip: None,
                        kind: SettingKind::F64Drag {
                            min: 20.0,
                            max: 800.0,
                            speed: 1.0,
                            default: self.settings.sigma_max_nm,
                        },
                    },
                ],
            }],
        }
    }

    fn get_setting(&self, key: &str) -> Option<Value> {
        match key {
            "method" => Some(json!(self.settings.method.index())),
            "history_depth" => Some(json!(self.settings.history_depth)),
            "sigma_min_nm" => Some(json!(self.settings.sigma_min_nm)),
            "sigma_max_nm" => Some(json!(self.settings.sigma_max_nm)),
            "nm_per_pixel" => Some(json!(self.settings.nm_per_pixel)),
            _ => None,
        }
    }

    fn set_setting(&mut self, key: &str, value: Value) -> Result<(), String> {
        match key {
            "method" => {
                let Some(value) = Self::parse_usize(value) else {
                    return Err("method must be an integer".into());
                };
                let method = FocusMethod::from_index(value);
                if method != self.settings.method {
                    self.settings.method = method;
                    self.clear_history();
                }
            }
            "history_depth" => {
                let Some(value) = Self::parse_usize(value) else {
                    return Err("history_depth must be an integer".into());
                };
                self.settings.history_depth = value.clamp(16, 360);
                while self.history.len() > self.settings.history_depth {
                    self.history.pop_front();
                }
            }
            "sigma_min_nm" => {
                let Some(value) = value.as_f64() else {
                    return Err("sigma_min_nm must be numeric".into());
                };
                self.settings.sigma_min_nm = value.clamp(10.0, 500.0);
            }
            "sigma_max_nm" => {
                let Some(value) = value.as_f64() else {
                    return Err("sigma_max_nm must be numeric".into());
                };
                self.settings.sigma_max_nm = value.clamp(20.0, 800.0);
            }
            "nm_per_pixel" => {
                let Some(value) = value.as_f64() else {
                    return Err("nm_per_pixel must be numeric".into());
                };
                self.settings.nm_per_pixel = value.clamp(20.0, 150.0);
            }
            _ => return Err(format!("unknown setting: {key}")),
        }
        Ok(())
    }

    fn status_entries(&self) -> Vec<StatusEntry> {
        let (quality_label, quality_color) = self.quality_indicator();
        vec![
            StatusEntry::Text(self.current_metric_label()),
            StatusEntry::LabeledValue {
                label: "Focus quality".into(),
                value: quality_label.into(),
                color: Some(quality_color),
            },
            StatusEntry::Text(format!("Trend: {}", self.trend_label())),
            StatusEntry::Text(self.last_status.clone()),
            StatusEntry::Sparkline {
                label: "History".into(),
                values: self.history.iter().copied().collect(),
                lower_is_better: self.settings.method.lower_is_better(),
            },
        ]
    }
}

fn fft_focus_metric(frame: &PluginFrame<'_>) -> Option<f64> {
    let (width, height, mut buffer) = downsample_frame(frame, 192);
    if width < 8 || height < 8 {
        return None;
    }

    let mean = buffer.iter().copied().sum::<f32>() / buffer.len() as f32;
    for value in &mut buffer {
        *value -= mean;
    }
    let signal_energy: f32 = buffer.iter().map(|value| value * value).sum();
    if signal_energy <= 1e-6 {
        return None;
    }

    let mut planner = FftPlanner::<f32>::new();
    let fft_width = planner.plan_fft_forward(width);
    let fft_height = planner.plan_fft_forward(height);
    let mut spectrum: Vec<Complex32> = buffer
        .into_iter()
        .map(|value| Complex32::new(value, 0.0))
        .collect();

    for row in 0..height {
        fft_width.process(&mut spectrum[row * width..(row + 1) * width]);
    }

    let mut column = vec![Complex32::new(0.0, 0.0); height];
    for x in 0..width {
        for y in 0..height {
            column[y] = spectrum[y * width + x];
        }
        fft_height.process(&mut column);
        for y in 0..height {
            spectrum[y * width + x] = column[y];
        }
    }

    let mut power_sum = 0.0f64;
    let mut count = 0usize;
    for y in 0..height {
        let fy = normalized_frequency(y, height);
        for x in 0..width {
            let fx = normalized_frequency(x, width);
            let radius = (fx * fx + fy * fy).sqrt();
            if !(0.25..=0.9).contains(&radius) {
                continue;
            }
            let power = spectrum[y * width + x].norm_sqr() as f64;
            power_sum += power;
            count += 1;
        }
    }

    if count == 0 {
        None
    } else {
        Some(power_sum / count as f64)
    }
}

fn normalized_frequency(index: usize, size: usize) -> f32 {
    let half = size as f32 / 2.0;
    let centered = if index <= size / 2 {
        index as f32
    } else {
        index as f32 - size as f32
    };
    centered / half.max(1.0)
}

fn downsample_frame(frame: &PluginFrame<'_>, max_dim: usize) -> (usize, usize, Vec<f32>) {
    let width = frame.width() as usize;
    let height = frame.height() as usize;
    let step_x = width.div_ceil(max_dim).max(1);
    let step_y = height.div_ceil(max_dim).max(1);
    let down_width = width.div_ceil(step_x);
    let down_height = height.div_ceil(step_y);
    let mut output = vec![0.0f32; down_width * down_height];

    for oy in 0..down_height {
        let src_y0 = oy * step_y;
        let src_y1 = ((oy + 1) * step_y).min(height);
        for ox in 0..down_width {
            let src_x0 = ox * step_x;
            let src_x1 = ((ox + 1) * step_x).min(width);
            let mut sum = 0.0f32;
            let mut count = 0usize;
            for y in src_y0..src_y1 {
                for x in src_x0..src_x1 {
                    sum += frame.pixels()[y * width + x] as f32;
                    count += 1;
                }
            }
            output[oy * down_width + ox] = if count == 0 { 0.0 } else { sum / count as f32 };
        }
    }

    (down_width, down_height, output)
}

export_plugin!(FocusMetricsPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use augur_plugin_api::{FfiPreviewFrame, FfiSlice};

    #[test]
    fn downsample_preserves_nonzero_signal() {
        let pixels = {
            let mut pixels = vec![0u16; 16 * 16];
            pixels[5 * 16 + 6] = 200;
            pixels
        };
        let frame = FfiPreviewFrame {
            width: 16,
            height: 16,
            pixels: FfiSlice::from_slice(&pixels),
            events: FfiSlice::default(),
            window_start_us: 0,
            window_end_us: 1_000,
        };
        let frame = PluginFrame::new(&frame);

        let (_w, _h, values) = downsample_frame(&frame, 8);
        assert!(values.iter().any(|value| *value > 0.0));
    }
}
