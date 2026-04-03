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
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalysisOutput {
    pub overlays: Vec<Overlay>,
    pub warnings: Vec<AnalysisWarning>,
}
