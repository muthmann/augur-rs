mod focus_metrics;
mod hotpixel;
mod localization;
mod roi_grid;
pub mod types;

use crate::plugin::AnalysisPlugin;

pub use focus_metrics::FocusMetricsPlugin;
pub use hotpixel::HotpixelPlugin;
pub use localization::LocalizationPlugin;
pub use roi_grid::RoiGridPlugin;

pub fn create_all_plugins() -> Vec<Box<dyn AnalysisPlugin>> {
    vec![
        Box::new(HotpixelPlugin::default()),
        Box::new(RoiGridPlugin::default()),
        Box::new(LocalizationPlugin::default()),
        Box::new(FocusMetricsPlugin::default()),
    ]
}
