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

    const CANDIDATES_UM: [f64; 8] = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0];
    let mut best = None;
    for candidate in CANDIDATES_UM {
        let sensor_pixels = candidate * 1_000.0 / nm_per_pixel;
        let screen_width = sensor_pixels as f32 * screen_pixels_per_sensor_pixel;
        if !(48.0..=180.0).contains(&screen_width) {
            continue;
        }
        best = Some(ScaleBarSpec {
            length_um: candidate,
            label: scale_bar_label(candidate),
            screen_width,
        });
    }

    best.or_else(|| {
        CANDIDATES_UM.first().map(|candidate| ScaleBarSpec {
            length_um: *candidate,
            label: scale_bar_label(*candidate),
            screen_width: (*candidate * 1_000.0 / nm_per_pixel) as f32
                * screen_pixels_per_sensor_pixel,
        })
    })
}

fn scale_bar_label(length_um: f64) -> String {
    if (length_um.fract()).abs() < f64::EPSILON {
        format!("{length_um:.0} µm")
    } else {
        format!("{length_um:.1} µm")
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
}
