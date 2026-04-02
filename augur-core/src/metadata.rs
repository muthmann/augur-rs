use std::{collections::BTreeMap, fs, path::Path};

use gethostname::gethostname;
use serde::{Deserialize, Serialize};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{camera::DeviceInfo, config::CameraConfig, CameraError, Result};

const HEADER_KEYS: &[&str] = &[
    "serial_number",
    "system_id",
    "firmware_version",
    "sensor_compatible",
    "augur_version",
    "recording_date",
    "recording_hostname",
    "pixel_pitch_nm",
];

const NON_HEADER_KEYS: &[&str] = &[
    "recording_duration_us",
    "total_events",
    "experiment_id",
    "operator",
    "notes",
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl RecordingAnnotations {
    pub fn normalized(mut self) -> Self {
        self.experiment_id = normalize_optional(self.experiment_id);
        self.operator = normalize_optional(self.operator);
        self.notes = normalize_optional(self.notes);
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor_compatible: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub augur_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_pitch_nm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_duration_us: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_events: Option<u64>,
    #[serde(default, skip_serializing_if = "annotations_are_empty")]
    pub annotations: RecordingAnnotations,
    #[serde(default, flatten)]
    pub extra: BTreeMap<String, String>,
}

impl RecordingMetadata {
    pub fn from_context(device: &DeviceInfo, config: &CameraConfig) -> Self {
        Self {
            serial_number: normalize_optional(device.serial.clone()),
            system_id: build_system_id(device),
            firmware_version: normalize_optional(device.firmware.clone()),
            sensor_compatible: normalize_optional(device.compatible.clone()),
            augur_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            recording_date: recording_date_now_utc(),
            recording_hostname: normalize_optional(Some(
                gethostname().to_string_lossy().into_owned(),
            )),
            pixel_pitch_nm: config
                .global
                .nm_per_pixel
                .is_finite()
                .then_some(config.global.nm_per_pixel),
            ..Self::default()
        }
    }

    pub fn with_annotations(mut self, annotations: RecordingAnnotations) -> Self {
        self.annotations = annotations.normalized();
        self
    }

    pub fn to_header_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        push_header_line(&mut lines, "serial_number", self.serial_number.as_deref());
        push_header_line(&mut lines, "system_id", self.system_id.as_deref());
        push_header_line(
            &mut lines,
            "firmware_version",
            self.firmware_version.as_deref(),
        );
        push_header_line(
            &mut lines,
            "sensor_compatible",
            self.sensor_compatible.as_deref(),
        );
        push_header_line(&mut lines, "augur_version", self.augur_version.as_deref());
        push_header_line(&mut lines, "recording_date", self.recording_date.as_deref());
        push_header_line(
            &mut lines,
            "recording_hostname",
            self.recording_hostname.as_deref(),
        );
        if let Some(pixel_pitch_nm) = self.pixel_pitch_nm {
            lines.push(format!("% pixel_pitch_nm {pixel_pitch_nm}"));
        }
        for (key, value) in &self.extra {
            if HEADER_KEYS.contains(&key.as_str()) || NON_HEADER_KEYS.contains(&key.as_str()) {
                continue;
            }
            lines.push(format!("% {key} {value}"));
        }
        lines
    }

    pub fn from_header_lines<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut metadata = Self::default();
        for (key, value) in pairs {
            let key = key.into();
            let value = value.into();
            match key.as_str() {
                "serial_number" => metadata.serial_number = normalize_optional(Some(value)),
                "system_id" => metadata.system_id = normalize_optional(Some(value)),
                "firmware_version" => metadata.firmware_version = normalize_optional(Some(value)),
                "sensor_compatible" => {
                    metadata.sensor_compatible = normalize_optional(Some(value));
                }
                "augur_version" => metadata.augur_version = normalize_optional(Some(value)),
                "recording_date" => metadata.recording_date = normalize_optional(Some(value)),
                "recording_hostname" => {
                    metadata.recording_hostname = normalize_optional(Some(value));
                }
                "pixel_pitch_nm" => match value.trim().parse::<f64>() {
                    Ok(pixel_pitch_nm) if pixel_pitch_nm.is_finite() => {
                        metadata.pixel_pitch_nm = Some(pixel_pitch_nm);
                    }
                    _ => {
                        metadata.extra.insert(key, value);
                    }
                },
                _ => {
                    metadata.extra.insert(key, value);
                }
            }
        }
        metadata
    }

    pub fn update_timing(&mut self, recording_duration_us: Option<u64>, total_events: u64) {
        self.recording_duration_us = Some(recording_duration_us.unwrap_or(0));
        self.total_events = Some(total_events);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingSidecar {
    #[serde(flatten)]
    pub config: CameraConfig,
    #[serde(default)]
    pub metadata: RecordingMetadata,
}

impl RecordingSidecar {
    pub fn new(config: CameraConfig, metadata: RecordingMetadata) -> Self {
        Self { config, metadata }
    }

    pub fn save_to_path(&self, path: impl AsRef<Path>) -> Result<()> {
        let encoded = toml::to_string_pretty(self)
            .map_err(|e| CameraError::Other(format!("failed to encode TOML: {e}")))?;
        fs::write(path, encoded)?;
        Ok(())
    }
}

fn annotations_are_empty(annotations: &RecordingAnnotations) -> bool {
    annotations == &RecordingAnnotations::default()
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then_some(trimmed.to_owned())
    })
}

fn recording_date_now_utc() -> Option<String> {
    OffsetDateTime::now_utc().format(&Rfc3339).ok()
}

fn build_system_id(device: &DeviceInfo) -> Option<String> {
    match (
        normalize_optional(Some(device.vendor.clone())),
        normalize_optional(Some(device.model.clone())),
    ) {
        (Some(vendor), Some(model)) => Some(format!("{vendor} {model}")),
        (Some(vendor), None) => Some(vendor),
        (None, Some(model)) => Some(model),
        (None, None) => None,
    }
}

fn push_header_line(lines: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        lines.push(format!("% {key} {value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_pairs(lines: &[String]) -> Vec<(String, String)> {
        lines
            .iter()
            .map(|line| {
                let rest = line
                    .strip_prefix("% ")
                    .expect("header lines must start with `% `");
                let (key, value) = rest
                    .split_once(' ')
                    .expect("header lines must contain a key and value");
                (key.to_owned(), value.to_owned())
            })
            .collect()
    }

    #[test]
    fn round_trips_header_fields_and_unknown_extra_lines() {
        let mut metadata = RecordingMetadata {
            serial_number: Some("00a1b2c3d4e5f678".into()),
            system_id: Some("Prophesee EVK4".into()),
            firmware_version: Some("0x040200".into()),
            sensor_compatible: Some("imx636,ccam5_gen42".into()),
            augur_version: Some("0.2.0".into()),
            recording_date: Some("2026-04-01T14:30:00Z".into()),
            recording_hostname: Some("lab-workstation-03".into()),
            pixel_pitch_nm: Some(4_860.0),
            recording_duration_us: Some(25_000_000),
            total_events: Some(4_200_000),
            annotations: RecordingAnnotations {
                experiment_id: Some("exp-42".into()),
                operator: Some("Ada".into()),
                notes: Some("test".into()),
            },
            ..RecordingMetadata::default()
        };
        metadata.extra.insert("beamline".into(), "B03".into());

        let header_lines = metadata.to_header_lines();
        assert!(header_lines.contains(&"% beamline B03".to_owned()));
        assert!(!header_lines
            .iter()
            .any(|line| line.contains("recording_duration_us")));
        assert!(!header_lines
            .iter()
            .any(|line| line.contains("experiment_id")));

        let parsed = RecordingMetadata::from_header_lines(header_pairs(&header_lines));
        assert_eq!(parsed.serial_number, metadata.serial_number);
        assert_eq!(parsed.system_id, metadata.system_id);
        assert_eq!(parsed.firmware_version, metadata.firmware_version);
        assert_eq!(parsed.sensor_compatible, metadata.sensor_compatible);
        assert_eq!(parsed.augur_version, metadata.augur_version);
        assert_eq!(parsed.recording_date, metadata.recording_date);
        assert_eq!(parsed.recording_hostname, metadata.recording_hostname);
        assert_eq!(parsed.pixel_pitch_nm, metadata.pixel_pitch_nm);
        assert_eq!(parsed.extra.get("beamline"), Some(&"B03".to_owned()));
        assert_eq!(parsed.recording_duration_us, None);
        assert_eq!(parsed.total_events, None);
        assert_eq!(parsed.annotations, RecordingAnnotations::default());
    }

    #[test]
    fn sidecar_toml_keeps_camera_config_compatible() {
        let config = CameraConfig::default();
        let mut metadata = RecordingMetadata::default();
        metadata.update_timing(Some(25_000_000), 4_200_000);
        metadata.annotations = RecordingAnnotations {
            operator: Some("Ada".into()),
            ..RecordingAnnotations::default()
        };
        let sidecar = RecordingSidecar::new(config.clone(), metadata);

        let encoded = toml::to_string_pretty(&sidecar).expect("sidecar must encode");
        let decoded_config: CameraConfig =
            toml::from_str(&encoded).expect("camera config must ignore metadata table");
        let decoded_toml: toml::Value = toml::from_str(&encoded).expect("TOML must parse");

        assert_eq!(decoded_config.global, config.global);
        assert_eq!(
            decoded_toml["metadata"]["total_events"].as_integer(),
            Some(4_200_000)
        );
        assert_eq!(
            decoded_toml["metadata"]["annotations"]["operator"].as_str(),
            Some("Ada")
        );
    }
}
