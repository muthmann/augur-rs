use egui::Color32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleBarPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScaleBarSettings {
    pub show: bool,
    pub position: ScaleBarPosition,
    pub color: Color32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScaleBarSpec {
    pub length_um: f64,
    pub label: String,
    pub screen_width: f32,
}

impl Default for ScaleBarSettings {
    fn default() -> Self {
        Self {
            show: true,
            position: ScaleBarPosition::BottomRight,
            color: Color32::WHITE,
        }
    }
}

pub fn compute_scale_bar(
    nm_per_pixel: f64,
    screen_pixels_per_sensor_pixel: f32,
) -> Option<ScaleBarSpec> {
    if nm_per_pixel <= 0.0 || screen_pixels_per_sensor_pixel <= 0.0 {
        return None;
    }

    const MIN_SCREEN_WIDTH: f32 = 48.0;
    const MAX_SCREEN_WIDTH: f32 = 180.0;
    const TARGET_SCREEN_WIDTH: f32 = 96.0;
    const MANTISSAS: [f64; 3] = [1.0, 2.0, 5.0];

    let candidates = (-1..=6).flat_map(|exp| {
        MANTISSAS
            .into_iter()
            .map(move |mantissa| mantissa * 10f64.powi(exp))
    });

    let mut best_in_range: Option<ScaleBarSpec> = None;
    let mut nearest_to_target: Option<ScaleBarSpec> = None;

    for candidate in candidates {
        let spec = scale_bar_spec(candidate, nm_per_pixel, screen_pixels_per_sensor_pixel);

        if nearest_to_target.as_ref().is_none_or(|best| {
            (spec.screen_width - TARGET_SCREEN_WIDTH).abs()
                < (best.screen_width - TARGET_SCREEN_WIDTH).abs()
        }) {
            nearest_to_target = Some(spec.clone());
        }

        if (MIN_SCREEN_WIDTH..=MAX_SCREEN_WIDTH).contains(&spec.screen_width)
            && best_in_range.as_ref().is_none_or(|best| {
                (spec.screen_width - TARGET_SCREEN_WIDTH).abs()
                    < (best.screen_width - TARGET_SCREEN_WIDTH).abs()
            })
        {
            best_in_range = Some(spec);
        }
    }

    best_in_range.or(nearest_to_target)
}

fn scale_bar_label(length_um: f64) -> String {
    if (length_um.fract()).abs() < f64::EPSILON {
        format!("{length_um:.0} µm")
    } else {
        format!("{length_um:.1} µm")
    }
}

fn scale_bar_spec(
    length_um: f64,
    nm_per_pixel: f64,
    screen_pixels_per_sensor_pixel: f32,
) -> ScaleBarSpec {
    let sensor_pixels = length_um * 1_000.0 / nm_per_pixel;
    let screen_width = sensor_pixels as f32 * screen_pixels_per_sensor_pixel;
    ScaleBarSpec {
        length_um,
        label: scale_bar_label(length_um),
        screen_width,
    }
}

#[cfg(test)]
mod tests {
    use super::compute_scale_bar;

    #[test]
    fn picks_reasonable_screen_width() {
        let bar = compute_scale_bar(100.0, 1.5).expect("scale bar should exist");
        assert!(bar.screen_width > 0.0);
    }

    #[test]
    fn keeps_scale_bar_visible_for_imx636_pixel_pitch() {
        let bar = compute_scale_bar(4_860.0, 0.625).expect("scale bar should exist");
        assert!((48.0..=180.0).contains(&bar.screen_width));
        assert!(bar.length_um >= 200.0);
    }
}
