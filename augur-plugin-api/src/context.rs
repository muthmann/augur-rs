use serde::{Deserialize, Serialize};

pub const CTX_LOCALIZATION_RESULTS: &str = "augur.localization.results";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Localization {
    pub x: f64,
    pub y: f64,
    pub sigma_x: f64,
    pub sigma_y: f64,
    pub amplitude: f64,
    pub background: f64,
    pub timestamp_us: u64,
    pub fit_error: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LocalizationResults {
    pub localizations: Vec<Localization>,
    pub frame_window_start_us: u64,
    pub frame_window_end_us: u64,
}
