mod roi_grid;

use crate::plugin::AnalysisPlugin;

pub use roi_grid::RoiGridPlugin;

pub fn create_all_plugins() -> Vec<Box<dyn AnalysisPlugin>> {
    vec![Box::new(RoiGridPlugin::default())]
}
