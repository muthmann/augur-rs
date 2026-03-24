#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RulerMeasurement {
    pub pixel_distance: f32,
    pub micrometers: f64,
    pub midpoint: (f32, f32),
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

    pub fn measurement(&self, nm_per_pixel: f64) -> Option<RulerMeasurement> {
        let (Some(start), Some(end)) = (self.start, self.end) else {
            return None;
        };
        let dx = f32::from(end.0) - f32::from(start.0);
        let dy = f32::from(end.1) - f32::from(start.1);
        let pixel_distance = (dx * dx + dy * dy).sqrt();
        Some(RulerMeasurement {
            pixel_distance,
            micrometers: f64::from(pixel_distance) * nm_per_pixel / 1_000.0,
            midpoint: (
                (f32::from(start.0) + f32::from(end.0)) * 0.5,
                (f32::from(start.1) + f32::from(end.1)) * 0.5,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::RulerTool;

    #[test]
    fn measurement_converts_pixels_to_micrometers() {
        let mut ruler = RulerTool::default();
        ruler.set_line((0, 0), (3, 4));
        let measurement = ruler.measurement(50.0).expect("measurement should exist");
        assert_eq!(measurement.pixel_distance, 5.0);
        assert!((measurement.micrometers - 0.25).abs() < f64::EPSILON);
    }
}
