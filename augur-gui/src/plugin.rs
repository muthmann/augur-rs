use std::{
    any::{Any, TypeId},
    collections::HashMap,
};

use augur_core::{
    analysis::AnalysisOutput,
    config::CameraConfig,
    pipeline::{CdEvent, PreviewFrame},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginInput {
    FrameOnly,
    RawEvents,
    DerivedData,
}

#[derive(Default)]
pub struct PluginContext {
    data: HashMap<TypeId, Box<dyn Any>>,
    pub raw_events: Option<Vec<CdEvent>>,
}

impl PluginContext {
    pub fn publish<T: Any + 'static>(&mut self, value: T) {
        self.data.insert(TypeId::of::<T>(), Box::new(value));
    }

    pub fn get<T: Any + 'static>(&self) -> Option<&T> {
        self.data
            .get(&TypeId::of::<T>())
            .and_then(|value| value.downcast_ref::<T>())
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.raw_events = None;
    }
}

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

    fn process_frame_with_context(
        &mut self,
        frame: &PreviewFrame,
        output: &mut AnalysisOutput,
        _ctx: &mut PluginContext,
    ) {
        self.process_frame(frame, output);
    }

    fn input_kind(&self) -> PluginInput {
        PluginInput::FrameOnly
    }

    fn dependencies(&self) -> &[&str] {
        &[]
    }

    fn reset(&mut self);
}
