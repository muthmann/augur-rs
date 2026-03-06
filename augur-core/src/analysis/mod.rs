pub mod hotpixel;
pub mod roi_grid;

use std::sync::Arc;

use crate::pipeline::PreviewFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pixel {
    pub x: u16,
    pub y: u16,
}

impl Pixel {
    pub const fn new(x: u16, y: u16) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubpixelMarker {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisWarning {
    pub source: String,
    pub severity: AnalysisSeverity,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    HighlightPixels {
        pixels: Vec<Pixel>,
        color: [u8; 4],
    },
    CrosshairMarkers {
        markers: Vec<SubpixelMarker>,
        color: [u8; 4],
        arm_len: u16,
    },
    RoiGrid {
        grid: Arc<roi_grid::RoiGrid>,
        highlight_top_n: usize,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalysisOutput {
    pub overlays: Vec<Overlay>,
    pub warnings: Vec<AnalysisWarning>,
}

pub trait Analyzer {
    fn name(&self) -> &str;
    fn process_frame(&mut self, frame: &PreviewFrame) -> AnalysisOutput;
    fn reset(&mut self);
}

#[derive(Default)]
pub struct AnalysisPipeline {
    analyzers: Vec<Box<dyn Analyzer>>,
}

impl AnalysisPipeline {
    pub fn new(analyzers: Vec<Box<dyn Analyzer>>) -> Self {
        Self { analyzers }
    }

    pub fn process_frame(&mut self, frame: &PreviewFrame) -> AnalysisOutput {
        let mut output = AnalysisOutput::default();
        for analyzer in &mut self.analyzers {
            let analyzer_output = analyzer.process_frame(frame);
            output.overlays.extend(analyzer_output.overlays);
            output.warnings.extend(analyzer_output.warnings);
        }
        output
    }

    pub fn reset(&mut self) {
        for analyzer in &mut self.analyzers {
            analyzer.reset();
        }
    }
}
