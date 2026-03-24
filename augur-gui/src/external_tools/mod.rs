mod imagej;

use augur_core::pipeline::PreviewFrame;

pub use imagej::ImageJBridge;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalToolStatus {
    Disconnected,
    Connecting,
    Streaming,
    Error(String),
}

impl ExternalToolStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Disconnected => "Disconnected".into(),
            Self::Connecting => "Connecting".into(),
            Self::Streaming => "Streaming".into(),
            Self::Error(err) => format!("Error: {err}"),
        }
    }

    pub fn is_streaming(&self) -> bool {
        matches!(self, Self::Streaming)
    }
}

pub trait ExternalTool: Send {
    fn name(&self) -> &str;
    fn status(&self) -> ExternalToolStatus;
    fn connect(&mut self) -> Result<(), String>;
    fn disconnect(&mut self);
    fn send_frame(&mut self, frame: &PreviewFrame, nm_per_pixel: f64) -> Result<(), String>;
}
