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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerShape {
    Point,
    Cross,
    Box,
    Ellipse,
    Diamond,
    FilledCircle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkerOverlayItem {
    pub x: f32,
    pub y: f32,
    pub shape: MarkerShape,
    pub size: f32,
    pub color: [u8; 4],
    pub timestamp_us: Option<u64>,
    pub stable_id: Option<String>,
    /// Explicit `(dataset_id, row_id)` that backs this marker. When set,
    /// the host uses it as the selection key on click; otherwise it falls
    /// back to `(overlay.dataset_id, stable_id)`.
    pub source_row: Option<(String, String)>,
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
    MarkerOverlay {
        markers: Vec<MarkerOverlayItem>,
        dataset_id: Option<String>,
        layer_id: Option<String>,
        source_label: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalysisOutput {
    pub overlays: Vec<Overlay>,
    pub warnings: Vec<AnalysisWarning>,
}
