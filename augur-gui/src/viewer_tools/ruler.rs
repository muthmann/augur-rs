use super::scale_bar::PixelScale;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RulerMeasurement {
    pub pixel_distance: f32,
    pub micrometers: f64,
    /// Whether `micrometers` came from a scale confirmed for this setup.
    pub calibrated: bool,
    pub midpoint: (f32, f32),
}

impl RulerMeasurement {
    /// `12.3 px · 59.78 µm (uncal.)` — the provenance is part of the reading,
    /// never a separate thing a caller can forget to render.
    pub fn label(&self) -> String {
        format!(
            "{:.1} px  \u{00B7}  {:.2} \u{00B5}m{}",
            self.pixel_distance,
            self.micrometers,
            super::scale_bar::calibration_suffix(self.calibrated)
        )
    }
}

#[derive(Debug, Default)]
pub struct RulerTool {
    pub start: Option<(u16, u16)>,
    pub end: Option<(u16, u16)>,
}

impl RulerTool {
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
    }

    pub fn set_line(&mut self, start: (u16, u16), end: (u16, u16)) {
        self.start = Some(start);
        self.end = Some(end);
    }

    pub fn measurement(&self, scale: PixelScale) -> Option<RulerMeasurement> {
        let (Some(start), Some(end)) = (self.start, self.end) else {
            return None;
        };
        let dx = f32::from(end.0) - f32::from(start.0);
        let dy = f32::from(end.1) - f32::from(start.1);
        let pixel_distance = (dx * dx + dy * dy).sqrt();
        Some(RulerMeasurement {
            pixel_distance,
            micrometers: scale.micrometers(f64::from(pixel_distance)),
            calibrated: scale.calibrated,
            midpoint: (
                (f32::from(start.0) + f32::from(end.0)) * 0.5,
                (f32::from(start.1) + f32::from(end.1)) * 0.5,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PixelScale, RulerTool};

    #[test]
    fn measurement_converts_pixels_to_micrometers() {
        let mut ruler = RulerTool::default();
        ruler.set_line((0, 0), (3, 4));
        let measurement = ruler
            .measurement(PixelScale {
                nm_per_pixel: 50.0,
                calibrated: true,
            })
            .expect("measurement should exist");
        assert_eq!(measurement.pixel_distance, 5.0);
        assert!((measurement.micrometers - 0.25).abs() < f64::EPSILON);
        assert_eq!(measurement.label(), "5.0 px  \u{00B7}  0.25 \u{00B5}m");
    }

    #[test]
    fn uncalibrated_measurements_say_so_in_their_own_label() {
        let mut ruler = RulerTool::default();
        ruler.set_line((0, 0), (3, 4));
        let measurement = ruler
            .measurement(PixelScale {
                nm_per_pixel: 50.0,
                calibrated: false,
            })
            .expect("measurement should exist");
        assert!(!measurement.calibrated);
        assert!(measurement.label().ends_with("(uncal.)"));
    }
}
