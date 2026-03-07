use augur_core::{analysis::AnalysisOutput, config::CameraConfig, pipeline::PreviewFrame};
use augur_plugin_api::PluginInput;

pub trait AnalysisPlugin {
    fn name(&self) -> &str;

    fn description(&self) -> &str {
        ""
    }

    fn enabled(&self) -> bool;

    fn set_enabled(&mut self, enabled: bool);

    /// Draw plugin settings. Returns true if `CameraConfig` was mutated.
    fn ui_settings(&mut self, ui: &mut egui::Ui, config: &mut CameraConfig) -> bool;

    /// Process a new preview frame. Called only when plugin is enabled.
    fn process_frame(&mut self, frame: &PreviewFrame, output: &mut AnalysisOutput);

    fn input_kind(&self) -> PluginInput {
        PluginInput::FrameOnly
    }

    fn dependencies(&self) -> &[&str] {
        &[]
    }

    fn reset(&mut self);
}
