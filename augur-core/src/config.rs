use std::{
    collections::HashSet,
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
    pub external_triggers: ExternalTriggerConfig,
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
            external_triggers: ExternalTriggerConfig::default(),
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
        if self.external_triggers.channel != 0 {
            return Err(CameraError::Config(
                "only external trigger channel 0 is supported".into(),
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
    /// Whether `nm_per_pixel` describes the sample plane of *this* setup.
    ///
    /// The default is the bare IMX636 pixel pitch, which is only the true
    /// sample-plane scale for direct detection. Behind any optics it is off by
    /// the magnification, so measurements derived from it (scale bar, ruler)
    /// stay labelled as uncalibrated until someone states otherwise.
    #[serde(default)]
    pub pixel_scale_calibrated: bool,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub acq_time_ms: u64,
    pub event_store_budget_mib: u64,
    pub preview_interval_ms: u64,
    pub point_cloud_interval_ms: u64,
    pub disk_writer_buffer_mib: u64,
    /// Persist the sensor-monitoring companion CSV with each camera recording.
    ///
    /// This is host-owned acquisition state. Keeping it in the configuration
    /// makes loading a named profile reproduce the recording behavior instead
    /// of only changing the visible camera registers.
    #[serde(default)]
    pub record_sensor_telemetry: bool,
}

impl Default for GlobalSettingsConfig {
    fn default() -> Self {
        Self {
            nm_per_pixel: 4_860.0,
            pixel_scale_calibrated: false,
            sensor_width: 1280,
            sensor_height: 720,
            acq_time_ms: 50,
            event_store_budget_mib: 100,
            preview_interval_ms: 33,
            point_cloud_interval_ms: 67,
            disk_writer_buffer_mib: 4,
            record_sensor_telemetry: false,
        }
    }
}

impl GlobalSettingsConfig {
    /// Return the effective host-safe values used by the runtime.
    pub fn normalized(mut self) -> Self {
        self.nm_per_pixel = self.nm_per_pixel.max(1.0);
        self.sensor_width = self.sensor_width.max(1);
        self.sensor_height = self.sensor_height.max(1);
        self.acq_time_ms = self.acq_time_ms.max(1);
        self.event_store_budget_mib = self.event_store_budget_mib.max(1);
        self.preview_interval_ms = self.preview_interval_ms.max(10);
        self.point_cloud_interval_ms = self.point_cloud_interval_ms.max(20);
        self.disk_writer_buffer_mib = self.disk_writer_buffer_mib.max(1);
        self
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

    /// Resolve explicit coordinates and an optional bitfield file into one
    /// immutable list of masked pixels.
    pub fn resolved_masked_pixels(&self, width: u16, height: u16) -> Result<Vec<(u16, u16)>> {
        self.validate(width, height)?;
        let mut pixels = self.masked_pixels.clone();

        if let Some(path) = &self.mask_file {
            let bytes = fs::read(path)?;
            let expected_size = (width as usize * height as usize).div_ceil(8);
            if bytes.len() != expected_size {
                return Err(CameraError::Config(format!(
                    "mask file '{}' has {} bytes, expected {} for {}x{} bitfield",
                    path.display(),
                    bytes.len(),
                    expected_size,
                    width,
                    height
                )));
            }

            for y in 0..height {
                for x in 0..width {
                    let index = y as usize * width as usize + x as usize;
                    if (bytes[index / 8] >> (index % 8)) & 1 == 1 {
                        pixels.push((x, y));
                    }
                }
            }
        }

        let mut seen = HashSet::new();
        pixels.retain(|pixel| seen.insert(*pixel));
        Ok(pixels)
    }

    pub fn to_bitfield(&self, width: u16, height: u16) -> Result<Vec<u8>> {
        let total_bits = width as usize * height as usize;
        let mut bits = vec![0_u8; total_bits.div_ceil(8)];
        for (x, y) in self.resolved_masked_pixels(width, height)? {
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

/// External trigger input (EVK4 TRIG_IN). When enabled, the sensor inserts
/// EVT3 `EXT_TRIGGER` events into the stream on each edge of the selected
/// channel, timestamped on the same clock as CD events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExternalTriggerConfig {
    pub enabled: bool,
    pub channel: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temporary_mask_path(label: &str) -> PathBuf {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "augur-mask-{label}-{}-{}.bin",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

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
    fn rejects_unsupported_external_trigger_channel() {
        let cfg = CameraConfig {
            external_triggers: ExternalTriggerConfig {
                enabled: true,
                channel: 1,
            },
            ..CameraConfig::default()
        };

        let err = cfg.validate(1280, 720).expect_err("must reject");
        assert!(err.to_string().contains("channel 0"));
    }

    #[test]
    fn pixel_mask_resolution_merges_and_deduplicates_file_pixels() {
        let path = temporary_mask_path("resolution");
        // 4x2 pixels: file masks (1,0) and (3,1).
        fs::write(&path, [0b1000_0010]).expect("mask fixture");
        let mask = PixelMaskConfig {
            masked_pixels: vec![(1, 0), (2, 1)],
            mask_file: Some(path.clone()),
        };

        assert_eq!(
            mask.resolved_masked_pixels(4, 2).expect("resolved mask"),
            vec![(1, 0), (2, 1), (3, 1)]
        );
        assert_eq!(
            mask.to_bitfield(4, 2).expect("resolved bitfield"),
            vec![0b1100_0010]
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn pixel_mask_resolution_rejects_wrong_bitfield_size() {
        let path = temporary_mask_path("size");
        fs::write(&path, [0_u8; 2]).expect("mask fixture");
        let mask = PixelMaskConfig {
            masked_pixels: Vec::new(),
            mask_file: Some(path.clone()),
        };

        let error = mask
            .resolved_masked_pixels(4, 2)
            .expect_err("wrong-sized bitfield must fail");
        assert!(error.to_string().contains("expected 1"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn global_settings_round_trip_through_toml() {
        let cfg = CameraConfig {
            global: GlobalSettingsConfig {
                nm_per_pixel: 42.5,
                pixel_scale_calibrated: true,
                sensor_width: 640,
                sensor_height: 480,
                acq_time_ms: 75,
                event_store_budget_mib: 256,
                preview_interval_ms: 40,
                point_cloud_interval_ms: 90,
                disk_writer_buffer_mib: 8,
                record_sensor_telemetry: true,
            },
            ..CameraConfig::default()
        };

        let encoded = toml::to_string_pretty(&cfg).expect("camera config must serialize");
        let decoded: CameraConfig = toml::from_str(&encoded).expect("camera config must parse");

        assert_eq!(decoded.global, cfg.global);
    }

    #[test]
    fn global_settings_normalization_matches_runtime_minima() {
        let normalized = GlobalSettingsConfig {
            nm_per_pixel: 0.0,
            sensor_width: 0,
            sensor_height: 0,
            acq_time_ms: 0,
            event_store_budget_mib: 0,
            preview_interval_ms: 0,
            point_cloud_interval_ms: 0,
            disk_writer_buffer_mib: 0,
            ..GlobalSettingsConfig::default()
        }
        .normalized();

        assert_eq!(normalized.nm_per_pixel, 1.0);
        assert_eq!(normalized.sensor_width, 1);
        assert_eq!(normalized.sensor_height, 1);
        assert_eq!(normalized.acq_time_ms, 1);
        assert_eq!(normalized.event_store_budget_mib, 1);
        assert_eq!(normalized.preview_interval_ms, 10);
        assert_eq!(normalized.point_cloud_interval_ms, 20);
        assert_eq!(normalized.disk_writer_buffer_mib, 1);
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
        // A config written before calibration provenance existed cannot claim
        // to be calibrated.
        assert!(!cfg.global.pixel_scale_calibrated);
        assert_eq!(cfg.external_triggers, ExternalTriggerConfig::default());
    }
}
