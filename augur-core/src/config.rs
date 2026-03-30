use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{CameraError, Result};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraConfig {
    pub biases: BiasConfig,
    pub roi: RoiConfig,
    pub pixel_mask: PixelMaskConfig,
    pub digital_filter: DigitalFilterConfig,
    #[serde(default)]
    pub global: GlobalSettingsConfig,
}

impl Default for CameraConfig {
    fn default() -> Self {
        Self {
            biases: BiasConfig::default(),
            roi: RoiConfig::full_frame(),
            pixel_mask: PixelMaskConfig::default(),
            digital_filter: DigitalFilterConfig::default(),
            global: GlobalSettingsConfig::default(),
        }
    }
}

impl CameraConfig {
    pub fn validate(&self, max_width: u16, max_height: u16) -> Result<()> {
        self.roi.validate(max_width, max_height)?;
        self.pixel_mask.validate(max_width, max_height)?;
        if self.digital_filter.stc_enabled && self.digital_filter.trail_enabled {
            return Err(CameraError::Config(
                "STC and Trail cannot both be enabled".into(),
            ));
        }
        if self.digital_filter.stc_threshold_us == 0 {
            return Err(CameraError::Config(
                "stc_threshold_us must be greater than 0".into(),
            ));
        }
        Ok(())
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(|e| CameraError::Config(format!("failed to parse TOML: {e}")))
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let encoded = toml::to_string_pretty(self)
            .map_err(|e| CameraError::Other(format!("failed to encode TOML: {e}")))?;
        fs::write(path, encoded)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSettingsConfig {
    pub nm_per_pixel: f64,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub acq_time_ms: u64,
    pub event_store_budget_mib: u64,
    pub preview_interval_ms: u64,
    pub point_cloud_interval_ms: u64,
    pub disk_writer_buffer_mib: u64,
}

impl Default for GlobalSettingsConfig {
    fn default() -> Self {
        Self {
            nm_per_pixel: 65.0,
            sensor_width: 1280,
            sensor_height: 720,
            acq_time_ms: 50,
            event_store_budget_mib: 100,
            preview_interval_ms: 33,
            point_cloud_interval_ms: 67,
            disk_writer_buffer_mib: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BiasConfig {
    pub diff_on: i32,
    pub diff_off: i32,
    pub fo: i32,
    pub hpf: i32,
    pub refr: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RoiConfig {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl RoiConfig {
    pub fn full_frame() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 1280,
            height: 720,
        }
    }

    pub fn validate(&self, max_width: u16, max_height: u16) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(CameraError::Config("ROI width/height must be > 0".into()));
        }
        let x_ok = self.x.saturating_add(self.width) <= max_width;
        let y_ok = self.y.saturating_add(self.height) <= max_height;
        if !x_ok || !y_ok {
            return Err(CameraError::Config(format!(
                "ROI [{},{} {}x{}] exceeds sensor bounds {}x{}",
                self.x, self.y, self.width, self.height, max_width, max_height
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PixelMaskConfig {
    #[serde(default, alias = "hot_pixels")]
    pub masked_pixels: Vec<(u16, u16)>,
    pub mask_file: Option<PathBuf>,
}

impl PixelMaskConfig {
    pub fn validate(&self, max_width: u16, max_height: u16) -> Result<()> {
        for &(x, y) in &self.masked_pixels {
            if x >= max_width || y >= max_height {
                return Err(CameraError::Config(format!(
                    "masked pixel ({x},{y}) outside sensor bounds {}x{}",
                    max_width, max_height
                )));
            }
        }
        Ok(())
    }

    pub fn to_bitfield(&self, width: u16, height: u16) -> Result<Vec<u8>> {
        if let Some(path) = &self.mask_file {
            return std::fs::read(path).map_err(Into::into);
        }

        let total_bits = width as usize * height as usize;
        let mut bits = vec![0_u8; total_bits.div_ceil(8)];
        for &(x, y) in &self.masked_pixels {
            let idx = y as usize * width as usize + x as usize;
            let byte = idx / 8;
            let bit = idx % 8;
            bits[byte] |= 1_u8 << bit;
        }
        Ok(bits)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigitalFilterConfig {
    pub stc_enabled: bool,
    pub stc_threshold_us: u32,
    pub trail_enabled: bool,
}

impl Default for DigitalFilterConfig {
    fn default() -> Self {
        Self {
            stc_enabled: false,
            stc_threshold_us: 1_000,
            trail_enabled: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hot_pixels_alias() {
        let cfg: CameraConfig = toml::from_str(
            r#"
            [biases]
            diff_on = 0
            diff_off = 0
            fo = 0
            hpf = 0
            refr = 0

            [roi]
            x = 0
            y = 0
            width = 1280
            height = 720

            [pixel_mask]
            hot_pixels = [[10, 20], [30, 40]]

            [digital_filter]
            stc_enabled = false
            stc_threshold_us = 1000
            trail_enabled = false
            "#,
        )
        .expect("valid toml");

        assert_eq!(cfg.pixel_mask.masked_pixels, vec![(10, 20), (30, 40)]);
        assert_eq!(cfg.global, GlobalSettingsConfig::default());
    }

    #[test]
    fn rejects_conflicting_filter_config() {
        let cfg = CameraConfig {
            digital_filter: DigitalFilterConfig {
                stc_enabled: true,
                stc_threshold_us: 1_000,
                trail_enabled: true,
            },
            ..CameraConfig::default()
        };

        let err = cfg.validate(1280, 720).expect_err("must reject");
        assert!(err.to_string().contains("cannot both be enabled"));
    }

    #[test]
    fn global_settings_round_trip_through_toml() {
        let cfg = CameraConfig {
            global: GlobalSettingsConfig {
                nm_per_pixel: 42.5,
                sensor_width: 640,
                sensor_height: 480,
                acq_time_ms: 75,
                event_store_budget_mib: 256,
                preview_interval_ms: 40,
                point_cloud_interval_ms: 90,
                disk_writer_buffer_mib: 8,
            },
            ..CameraConfig::default()
        };

        let encoded = toml::to_string_pretty(&cfg).expect("camera config must serialize");
        let decoded: CameraConfig = toml::from_str(&encoded).expect("camera config must parse");

        assert_eq!(decoded.global, cfg.global);
    }

    #[test]
    fn old_toml_without_global_uses_defaults() {
        let cfg: CameraConfig = toml::from_str(
            r#"
            [biases]
            diff_on = 1
            diff_off = 2
            fo = 3
            hpf = 4
            refr = 5

            [roi]
            x = 0
            y = 0
            width = 1280
            height = 720

            [pixel_mask]
            masked_pixels = []

            [digital_filter]
            stc_enabled = false
            stc_threshold_us = 1000
            trail_enabled = false
            "#,
        )
        .expect("legacy toml without global must load");

        assert_eq!(cfg.global, GlobalSettingsConfig::default());
    }
}
