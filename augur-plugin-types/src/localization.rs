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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LocalizationTable {
    pub rows: Vec<LocalizationRow>,
    pub nm_per_pixel: f64,
    pub sensor_width: u16,
    pub sensor_height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LocalizationRow {
    pub id: u64,
    pub frame: u64,
    pub x_nm: f64,
    pub y_nm: f64,
    pub sigma_nm: f64,
    pub intensity: f64,
    pub offset: f64,
    pub uncertainty_xy_nm: f64,
    pub timestamp_us: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localization_results_round_trip_through_json() {
        let results = LocalizationResults {
            localizations: vec![Localization {
                x: 12.5,
                y: 7.25,
                sigma_x: 1.4,
                sigma_y: 1.6,
                amplitude: 42.0,
                background: 2.5,
                timestamp_us: 1_234,
                fit_error: 0.12,
            }],
            frame_window_start_us: 10,
            frame_window_end_us: 110,
        };

        let json = serde_json::to_vec(&results).expect("results must serialize");
        let decoded: LocalizationResults =
            serde_json::from_slice(&json).expect("results must deserialize");
        assert_eq!(decoded, results);
        assert_eq!(CTX_LOCALIZATION_RESULTS, "augur.localization.results");
    }

    #[test]
    fn localization_table_round_trips_through_json() {
        let table = LocalizationTable {
            rows: vec![LocalizationRow {
                id: 7,
                frame: 3,
                x_nm: 812.5,
                y_nm: 455.0,
                sigma_nm: 132.0,
                intensity: 4_200.0,
                offset: 14.0,
                uncertainty_xy_nm: 19.5,
                timestamp_us: 12_345,
            }],
            nm_per_pixel: 65.0,
            sensor_width: 1280,
            sensor_height: 720,
        };

        let json = serde_json::to_vec(&table).expect("table must serialize");
        let decoded: LocalizationTable =
            serde_json::from_slice(&json).expect("table must deserialize");
        assert_eq!(decoded, table);
    }
}
