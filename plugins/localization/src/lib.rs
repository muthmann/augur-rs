use augur_plugin_api::{
    export_plugin, AnalysisSeverity, FfiCdEvent, FfiSubpixelMarker, HostContext, HostOutput,
    Localization, LocalizationResults, Plugin, PluginFrame, PluginInput, SettingItem, SettingKind,
    SettingsSchema, SettingsSection, StatusEntry, CTX_LOCALIZATION_RESULTS,
};
use serde_json::{json, Value};

const KERNEL_G1: [f64; 5] = [1.0 / 16.0, 0.25, 3.0 / 8.0, 0.25, 1.0 / 16.0];
const KERNEL_G2: [f64; 9] = [
    1.0 / 16.0,
    0.0,
    0.25,
    0.0,
    3.0 / 8.0,
    0.0,
    0.25,
    0.0,
    1.0 / 16.0,
];
const OVERLAY_COLOR: [u8; 4] = [255, 210, 32, 220];
const MAX_SPOT_CANDIDATES: usize = 256;

#[derive(Debug, Clone)]
struct LocalizationSettings {
    threshold_factor: f64,
    fit_radius_px: usize,
    initial_sigma_px: f64,
    sigma_min_nm: f64,
    sigma_max_nm: f64,
    max_xy_uncertainty_nm: f64,
    nm_per_pixel: f64,
    show_overlay: bool,
}

impl Default for LocalizationSettings {
    fn default() -> Self {
        Self {
            threshold_factor: 1.5,
            fit_radius_px: 4,
            initial_sigma_px: 1.6,
            sigma_min_nm: 100.0,
            sigma_max_nm: 190.0,
            max_xy_uncertainty_nm: 35.0,
            nm_per_pixel: 65.0,
            show_overlay: true,
        }
    }
}

struct LocalizationPlugin {
    enabled: bool,
    settings: LocalizationSettings,
    last_candidate_count: usize,
    last_localization_count: usize,
    last_mean_background: Option<f64>,
    last_status: String,
}

impl Default for LocalizationPlugin {
    fn default() -> Self {
        Self {
            enabled: false,
            settings: LocalizationSettings::default(),
            last_candidate_count: 0,
            last_localization_count: 0,
            last_mean_background: None,
            last_status:
                "Enable the plugin to extract molecule candidates from the preview stream.".into(),
        }
    }
}

impl LocalizationPlugin {
    fn sigma_in_range_nm(&self, localization: &Localization) -> bool {
        let min_nm = self.settings.sigma_min_nm;
        let max_nm = self.settings.sigma_max_nm;
        let sigma_x_nm = localization.sigma_x * self.settings.nm_per_pixel;
        let sigma_y_nm = localization.sigma_y * self.settings.nm_per_pixel;
        sigma_x_nm >= min_nm && sigma_x_nm <= max_nm && sigma_y_nm >= min_nm && sigma_y_nm <= max_nm
    }

    fn xy_uncertainty_nm(&self, localization: &Localization) -> f64 {
        let sigma_mean_px = 0.5 * (localization.sigma_x + localization.sigma_y);
        let signal = localization.amplitude.abs().max(1.0).sqrt();
        sigma_mean_px / signal * self.settings.nm_per_pixel
    }

    fn analyze_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        raw_events: Option<&[FfiCdEvent]>,
        output: &mut HostOutput<'_>,
    ) -> LocalizationResults {
        let image = build_analysis_image(frame, raw_events);
        let width = frame.width() as usize;
        let height = frame.height() as usize;

        if image.is_empty() {
            self.last_candidate_count = 0;
            self.last_localization_count = 0;
            self.last_mean_background = None;
            self.last_status = "The preview frame is empty.".into();
            return LocalizationResults {
                localizations: Vec::new(),
                frame_window_start_us: frame.window_start_us(),
                frame_window_end_us: frame.window_end_us(),
            };
        }

        let v1 = smooth_image(&image, width, height, &KERNEL_G1);
        let v2 = smooth_image(&v1, width, height, &KERNEL_G2);
        let f1: Vec<f64> = image.iter().zip(&v1).map(|(a, b)| a - b).collect();
        let f2: Vec<f64> = v1.iter().zip(&v2).map(|(a, b)| a - b).collect();
        let sigma = standard_deviation(&f1);
        let threshold = self.settings.threshold_factor * sigma;
        let filtered: Vec<f64> = f2
            .iter()
            .map(|value| if *value > threshold { *value } else { 0.0 })
            .collect();

        let mut candidates = find_local_maxima(&filtered, width, height);
        candidates.sort_by(|a, b| b.2.total_cmp(&a.2));
        if candidates.len() > MAX_SPOT_CANDIDATES {
            candidates.truncate(MAX_SPOT_CANDIDATES);
        }
        self.last_candidate_count = candidates.len();

        let mut localizations = Vec::new();
        for (x, y, _) in candidates {
            let (com_x, com_y) = center_of_mass(&filtered, width, height, x, y, 1);
            let Some(mut localization) = fit_localization(
                &image,
                width,
                height,
                com_x,
                com_y,
                self.settings.fit_radius_px,
                self.settings.initial_sigma_px,
            ) else {
                continue;
            };

            localization.timestamp_us = estimate_timestamp_us(
                raw_events,
                frame.window_start_us(),
                frame.window_end_us(),
                localization.x,
                localization.y,
                self.settings.fit_radius_px as f64,
            );

            if !self.sigma_in_range_nm(&localization) {
                continue;
            }
            if self.xy_uncertainty_nm(&localization) > self.settings.max_xy_uncertainty_nm {
                continue;
            }
            if !localization.fit_error.is_finite() {
                continue;
            }

            localizations.push(localization);
        }

        self.last_localization_count = localizations.len();
        self.last_mean_background = if localizations.is_empty() {
            None
        } else {
            Some(
                localizations
                    .iter()
                    .map(|localization| localization.background)
                    .sum::<f64>()
                    / localizations.len() as f64,
            )
        };
        self.last_status = format!(
            "{} molecules detected this frame ({} spot candidates).",
            self.last_localization_count, self.last_candidate_count
        );

        if self.settings.show_overlay && !localizations.is_empty() {
            let markers: Vec<FfiSubpixelMarker> = localizations
                .iter()
                .map(|localization| FfiSubpixelMarker {
                    x: localization.x as f32,
                    y: localization.y as f32,
                })
                .collect();
            output.add_crosshair_markers(&markers, OVERLAY_COLOR, 5);
        }

        LocalizationResults {
            localizations,
            frame_window_start_us: frame.window_start_us(),
            frame_window_end_us: frame.window_end_us(),
        }
    }

    fn parse_usize(value: Value) -> Option<usize> {
        value.as_u64().and_then(|value| usize::try_from(value).ok())
    }

    fn warning(output: &mut HostOutput<'_>, message: &str) {
        output.add_warning("Molecule Localization", AnalysisSeverity::Warning, message);
    }
}

impl Plugin for LocalizationPlugin {
    fn name(&self) -> &'static str {
        "Molecule Localization"
    }

    fn description(&self) -> &'static str {
        "Wavelet-filtered spot detection with center-of-mass seeding and least-squares Gaussian fitting."
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
        self.last_candidate_count = 0;
        self.last_localization_count = 0;
        self.last_mean_background = None;
        self.last_status = "Waiting for the next preview frame.".into();
    }

    fn input_kind(&self) -> PluginInput {
        PluginInput::RawEvents
    }

    fn process_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        context: &mut HostContext<'_>,
    ) {
        let raw_events = if !context.raw_events().is_empty() {
            Some(context.raw_events())
        } else if !frame.events().is_empty() {
            Some(frame.events())
        } else {
            None
        };

        if raw_events.is_none() {
            Self::warning(
                output,
                "Raw-event transport is unavailable for this frame; falling back to preview counts.",
            );
        }

        let results = self.analyze_frame(frame, raw_events, output);
        if let Err(err) = context.publish(CTX_LOCALIZATION_RESULTS, &results) {
            output.add_warning(
                self.name(),
                AnalysisSeverity::Error,
                &format!("Failed to publish localization results: {err}"),
            );
        }
    }

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema {
            sections: vec![SettingsSection {
                label: "Localization".into(),
                description: Some(
                    "Wavelet filtering finds candidate emitters before the Gaussian fit rejects unstable detections."
                        .into(),
                ),
                default_open: true,
                items: vec![
                    SettingItem {
                        key: "threshold_factor".into(),
                        label: "Wavelet threshold n".into(),
                        tooltip: Some("Threshold multiplier applied to sigma(F1) before keeping F2 coefficients.".into()),
                        kind: SettingKind::F64Slider {
                            min: 0.5,
                            max: 4.0,
                            default: self.settings.threshold_factor,
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "fit_radius_px".into(),
                        label: "Fit radius".into(),
                        tooltip: Some("Radius of the Gaussian-fit ROI in pixels.".into()),
                        kind: SettingKind::I64Drag {
                            min: 2,
                            max: 8,
                            default: i64::try_from(self.settings.fit_radius_px).unwrap_or(4),
                        },
                    },
                    SettingItem {
                        key: "initial_sigma_px".into(),
                        label: "Initial sigma [px]".into(),
                        tooltip: None,
                        kind: SettingKind::F64Slider {
                            min: 0.8,
                            max: 3.5,
                            default: self.settings.initial_sigma_px,
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "nm_per_pixel".into(),
                        label: "Scale [nm/px]".into(),
                        tooltip: Some("Used to express sigma and uncertainty filters in nanometers.".into()),
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
                    SettingItem {
                        key: "max_xy_uncertainty_nm".into(),
                        label: "Max xy uncertainty [nm]".into(),
                        tooltip: None,
                        kind: SettingKind::F64Slider {
                            min: 5.0,
                            max: 120.0,
                            default: self.settings.max_xy_uncertainty_nm,
                            suffix: None,
                        },
                    },
                    SettingItem {
                        key: "show_overlay".into(),
                        label: "Show localization overlay".into(),
                        tooltip: None,
                        kind: SettingKind::Bool {
                            default: self.settings.show_overlay,
                        },
                    },
                ],
            }],
        }
    }

    fn get_setting(&self, key: &str) -> Option<Value> {
        match key {
            "threshold_factor" => Some(json!(self.settings.threshold_factor)),
            "fit_radius_px" => Some(json!(self.settings.fit_radius_px)),
            "initial_sigma_px" => Some(json!(self.settings.initial_sigma_px)),
            "sigma_min_nm" => Some(json!(self.settings.sigma_min_nm)),
            "sigma_max_nm" => Some(json!(self.settings.sigma_max_nm)),
            "max_xy_uncertainty_nm" => Some(json!(self.settings.max_xy_uncertainty_nm)),
            "nm_per_pixel" => Some(json!(self.settings.nm_per_pixel)),
            "show_overlay" => Some(json!(self.settings.show_overlay)),
            _ => None,
        }
    }

    fn set_setting(&mut self, key: &str, value: Value) -> Result<(), String> {
        match key {
            "threshold_factor" => {
                let Some(value) = value.as_f64() else {
                    return Err("threshold_factor must be numeric".into());
                };
                self.settings.threshold_factor = value.clamp(0.5, 4.0);
            }
            "fit_radius_px" => {
                let Some(value) = Self::parse_usize(value) else {
                    return Err("fit_radius_px must be an integer".into());
                };
                self.settings.fit_radius_px = value.clamp(2, 8);
            }
            "initial_sigma_px" => {
                let Some(value) = value.as_f64() else {
                    return Err("initial_sigma_px must be numeric".into());
                };
                self.settings.initial_sigma_px = value.clamp(0.8, 3.5);
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
            "max_xy_uncertainty_nm" => {
                let Some(value) = value.as_f64() else {
                    return Err("max_xy_uncertainty_nm must be numeric".into());
                };
                self.settings.max_xy_uncertainty_nm = value.clamp(5.0, 120.0);
            }
            "nm_per_pixel" => {
                let Some(value) = value.as_f64() else {
                    return Err("nm_per_pixel must be numeric".into());
                };
                self.settings.nm_per_pixel = value.clamp(20.0, 150.0);
            }
            "show_overlay" => {
                let Some(value) = value.as_bool() else {
                    return Err("show_overlay must be a bool".into());
                };
                self.settings.show_overlay = value;
            }
            _ => return Err(format!("unknown setting: {key}")),
        }

        Ok(())
    }

    fn status_entries(&self) -> Vec<StatusEntry> {
        let mut entries = vec![
            StatusEntry::Text(self.last_status.clone()),
            StatusEntry::Text(format!(
                "Current frame: {} accepted / {} candidates",
                self.last_localization_count, self.last_candidate_count
            )),
        ];
        if let Some(background) = self.last_mean_background {
            entries.push(StatusEntry::Text(format!(
                "Mean fitted background: {:.2}",
                background
            )));
        }
        entries
    }
}

fn build_analysis_image(frame: &PluginFrame<'_>, raw_events: Option<&[FfiCdEvent]>) -> Vec<f64> {
    if let Some(events) = raw_events.filter(|events| !events.is_empty()) {
        let mut image = vec![0.0; frame.pixels().len()];
        for event in events {
            if event.x >= frame.width() || event.y >= frame.height() {
                continue;
            }
            let idx = event.y as usize * frame.width() as usize + event.x as usize;
            let weight = event
                .timestamp
                .saturating_sub(frame.window_start_us())
                .max(1) as f64;
            if event.polarity != 0 {
                image[idx] += weight;
            } else {
                image[idx] -= weight;
            }
        }
        image
    } else {
        frame
            .pixels()
            .iter()
            .map(|&pixel| f64::from(pixel))
            .collect()
    }
}

fn smooth_image(input: &[f64], width: usize, height: usize, kernel: &[f64]) -> Vec<f64> {
    let radius = kernel.len() / 2;
    let mut horizontal = vec![0.0; input.len()];
    let mut output = vec![0.0; input.len()];

    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (offset, weight) in kernel.iter().enumerate() {
                let src_x = clamp_index(x as isize + offset as isize - radius as isize, width);
                acc += input[y * width + src_x] * *weight;
            }
            horizontal[y * width + x] = acc;
        }
    }

    for y in 0..height {
        for x in 0..width {
            let mut acc = 0.0;
            for (offset, weight) in kernel.iter().enumerate() {
                let src_y = clamp_index(y as isize + offset as isize - radius as isize, height);
                acc += horizontal[src_y * width + x] * *weight;
            }
            output[y * width + x] = acc;
        }
    }

    output
}

fn clamp_index(index: isize, limit: usize) -> usize {
    index.clamp(0, limit.saturating_sub(1) as isize) as usize
}

fn standard_deviation(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let variance = values
        .iter()
        .map(|value| {
            let diff = *value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

fn find_local_maxima(image: &[f64], width: usize, height: usize) -> Vec<(usize, usize, f64)> {
    let mut maxima = Vec::new();
    if width < 3 || height < 3 {
        return maxima;
    }

    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let value = image[y * width + x];
            if value <= 0.0 {
                continue;
            }
            let mut is_maximum = true;
            for ny in y - 1..=y + 1 {
                for nx in x - 1..=x + 1 {
                    if nx == x && ny == y {
                        continue;
                    }
                    if image[ny * width + nx] > value {
                        is_maximum = false;
                        break;
                    }
                }
                if !is_maximum {
                    break;
                }
            }
            if is_maximum {
                maxima.push((x, y, value));
            }
        }
    }

    maxima
}

fn center_of_mass(
    image: &[f64],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    radius: usize,
) -> (f64, f64) {
    let x0 = x.saturating_sub(radius);
    let x1 = (x + radius).min(width.saturating_sub(1));
    let y0 = y.saturating_sub(radius);
    let y1 = (y + radius).min(height.saturating_sub(1));

    let mut mass = 0.0;
    let mut weighted_x = 0.0;
    let mut weighted_y = 0.0;
    for yy in y0..=y1 {
        for xx in x0..=x1 {
            let weight = image[yy * width + xx].max(0.0);
            mass += weight;
            weighted_x += xx as f64 * weight;
            weighted_y += yy as f64 * weight;
        }
    }

    if mass <= 0.0 {
        (x as f64, y as f64)
    } else {
        (weighted_x / mass, weighted_y / mass)
    }
}

fn fit_localization(
    image: &[f64],
    width: usize,
    height: usize,
    com_x: f64,
    com_y: f64,
    fit_radius_px: usize,
    initial_sigma_px: f64,
) -> Option<Localization> {
    let cx = com_x.round() as isize;
    let cy = com_y.round() as isize;
    let radius = fit_radius_px as isize;
    if cx - radius < 0
        || cy - radius < 0
        || cx + radius >= width as isize
        || cy + radius >= height as isize
    {
        return None;
    }

    let mut samples = Vec::with_capacity((2 * fit_radius_px + 1).pow(2));
    let mut min_value = f64::INFINITY;
    let mut max_value = f64::NEG_INFINITY;
    for yy in cy - radius..=cy + radius {
        for xx in cx - radius..=cx + radius {
            let value = image[yy as usize * width + xx as usize];
            min_value = min_value.min(value);
            max_value = max_value.max(value);
            samples.push((xx as f64, yy as f64, value));
        }
    }

    let mut params = [
        com_x,
        com_y,
        initial_sigma_px,
        initial_sigma_px,
        (max_value - min_value).max(1.0),
        min_value,
    ];

    let mut last_error = f64::INFINITY;
    let mut damping = 1e-2;
    for _ in 0..15 {
        let (jtj, jtr, error) = gaussian_normal_equations(&samples, &params);
        if !error.is_finite() {
            return None;
        }

        let mut lhs = jtj;
        for (i, row) in lhs.iter_mut().enumerate() {
            row[i] += damping;
        }
        let delta = solve_linear_system(lhs, jtr)?;

        let candidate = [
            params[0] + delta[0],
            params[1] + delta[1],
            (params[2] + delta[2]).clamp(0.5, 6.0),
            (params[3] + delta[3]).clamp(0.5, 6.0),
            (params[4] + delta[4]).max(1e-3),
            params[5] + delta[5],
        ];
        let (_, _, candidate_error) = gaussian_normal_equations(&samples, &candidate);
        if candidate_error < error {
            params = candidate;
            last_error = candidate_error;
            damping *= 0.7;
        } else {
            damping *= 2.0;
        }

        let step_norm = delta.iter().map(|value| value * value).sum::<f64>().sqrt();
        if step_norm < 1e-3 {
            break;
        }
    }

    if !last_error.is_finite() {
        return None;
    }

    Some(Localization {
        x: params[0],
        y: params[1],
        sigma_x: params[2],
        sigma_y: params[3],
        amplitude: params[4],
        background: params[5],
        timestamp_us: 0,
        fit_error: (last_error / samples.len() as f64).sqrt(),
    })
}

fn gaussian_normal_equations(
    samples: &[(f64, f64, f64)],
    params: &[f64; 6],
) -> ([[f64; 6]; 6], [f64; 6], f64) {
    let mut jtj = [[0.0; 6]; 6];
    let mut jtr = [0.0; 6];
    let mut error = 0.0;

    let [x0, y0, sigma_x, sigma_y, amplitude, background] = *params;
    let sigma_x2 = sigma_x * sigma_x;
    let sigma_y2 = sigma_y * sigma_y;

    for (x, y, sample) in samples {
        let dx = *x - x0;
        let dy = *y - y0;
        let exponent = -0.5 * (dx * dx / sigma_x2 + dy * dy / sigma_y2);
        let g = exponent.exp();
        let model = background + amplitude * g;
        let residual = *sample - model;
        error += residual * residual;

        let jacobian = [
            amplitude * g * (dx / sigma_x2),
            amplitude * g * (dy / sigma_y2),
            amplitude * g * (dx * dx / sigma_x.powi(3)),
            amplitude * g * (dy * dy / sigma_y.powi(3)),
            g,
            1.0,
        ];

        for row in 0..6 {
            jtr[row] += jacobian[row] * residual;
            for col in 0..6 {
                jtj[row][col] += jacobian[row] * jacobian[col];
            }
        }
    }

    (jtj, jtr, error)
}

fn solve_linear_system(mut lhs: [[f64; 6]; 6], mut rhs: [f64; 6]) -> Option<[f64; 6]> {
    for pivot in 0..6 {
        let mut best_row = pivot;
        let mut best_value = lhs[pivot][pivot].abs();
        for (row, lhs_row) in lhs.iter().enumerate().skip(pivot + 1) {
            let candidate = lhs_row[pivot].abs();
            if candidate > best_value {
                best_value = candidate;
                best_row = row;
            }
        }
        if best_value < 1e-9 {
            return None;
        }
        if best_row != pivot {
            lhs.swap(pivot, best_row);
            rhs.swap(pivot, best_row);
        }

        let pivot_value = lhs[pivot][pivot];
        for col in pivot..6 {
            lhs[pivot][col] /= pivot_value;
        }
        rhs[pivot] /= pivot_value;

        for row in 0..6 {
            if row == pivot {
                continue;
            }
            let factor = lhs[row][pivot];
            if factor.abs() < 1e-12 {
                continue;
            }
            for col in pivot..6 {
                lhs[row][col] -= factor * lhs[pivot][col];
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }

    Some(rhs)
}

fn estimate_timestamp_us(
    raw_events: Option<&[FfiCdEvent]>,
    frame_window_start_us: u64,
    frame_window_end_us: u64,
    x: f64,
    y: f64,
    radius: f64,
) -> u64 {
    let Some(events) = raw_events else {
        return frame_window_start_us + (frame_window_end_us - frame_window_start_us) / 2;
    };

    let mut weighted_timestamp = 0.0;
    let mut weight_sum = 0.0;
    let radius2 = radius * radius;
    for event in events {
        let dx = f64::from(event.x) - x;
        let dy = f64::from(event.y) - y;
        let dist2 = dx * dx + dy * dy;
        if dist2 > radius2 {
            continue;
        }
        let weight = 1.0 / (1.0 + dist2);
        weighted_timestamp += event.timestamp as f64 * weight;
        weight_sum += weight;
    }

    if weight_sum <= 0.0 {
        frame_window_start_us + (frame_window_end_us - frame_window_start_us) / 2
    } else {
        (weighted_timestamp / weight_sum).round() as u64
    }
}

export_plugin!(LocalizationPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaussian_fit_recovers_center() {
        let width = 21;
        let height = 21;
        let mut image = vec![0.0; width * height];
        let x0 = 10.3;
        let y0 = 9.7;
        for y in 0..height {
            for x in 0..width {
                let dx = x as f64 - x0;
                let dy = y as f64 - y0;
                image[y * width + x] =
                    3.0 + 120.0 * (-0.5 * (dx * dx + dy * dy) / 1.6f64.powi(2)).exp();
            }
        }

        let localization = fit_localization(&image, width, height, x0, y0, 4, 1.6)
            .expect("fit must succeed on a clean synthetic spot");

        assert!((localization.x - x0).abs() < 0.2);
        assert!((localization.y - y0).abs() < 0.2);
        assert!((localization.sigma_x - 1.6).abs() < 0.25);
        assert!((localization.sigma_y - 1.6).abs() < 0.25);
    }
}
