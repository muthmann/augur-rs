mod imagej;

use augur_core::pipeline::PreviewFrame;

pub use imagej::{
    ImageJBridge, BUNDLED_IMAGEJ_PLUGIN_JAR, BUNDLED_IMAGEJ_PLUGIN_JAR_NAME,
    DEFAULT_IMAGEJ_BRIDGE_PORT,
};

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

/// How much of the preview series an external tool actually received.
///
/// A bridge is a sampled preview, not a capture path, so it is allowed to drop
/// frames under back-pressure — but the loss must be counted, never silent
/// (see `docs/features/recording-completeness.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalToolThroughput {
    pub frames_offered: u64,
    pub frames_dropped: u64,
}

impl ExternalToolThroughput {
    pub fn frames_delivered(&self) -> u64 {
        self.frames_offered.saturating_sub(self.frames_dropped)
    }

    pub fn has_gaps(&self) -> bool {
        self.frames_dropped > 0
    }

    /// `1200 sent · 37 dropped (3.0%)`, or just the sent count when the series
    /// is complete.
    pub fn label(&self) -> String {
        if !self.has_gaps() {
            return format!("{} frames sent", self.frames_offered);
        }
        let percent = if self.frames_offered == 0 {
            0.0
        } else {
            self.frames_dropped as f64 * 100.0 / self.frames_offered as f64
        };
        format!(
            "{} frames sent \u{00B7} {} dropped ({percent:.1}%)",
            self.frames_delivered(),
            self.frames_dropped
        )
    }
}

pub trait ExternalTool: Send {
    fn name(&self) -> &str;
    fn status(&self) -> ExternalToolStatus;
    fn connect(&mut self) -> Result<(), String>;
    fn disconnect(&mut self);
    fn send_frame(&mut self, frame: &PreviewFrame, nm_per_pixel: f64) -> Result<(), String>;
    fn throughput(&self) -> ExternalToolThroughput;
}

#[cfg(test)]
mod tests {
    use super::ExternalToolThroughput;

    #[test]
    fn complete_series_reports_only_the_sent_count() {
        let throughput = ExternalToolThroughput {
            frames_offered: 120,
            frames_dropped: 0,
        };
        assert!(!throughput.has_gaps());
        assert_eq!(throughput.label(), "120 frames sent");
    }

    #[test]
    fn dropped_frames_are_named_in_the_label() {
        let throughput = ExternalToolThroughput {
            frames_offered: 200,
            frames_dropped: 50,
        };
        assert!(throughput.has_gaps());
        assert_eq!(throughput.frames_delivered(), 150);
        assert_eq!(
            throughput.label(),
            "150 frames sent \u{00B7} 50 dropped (25.0%)"
        );
    }
}
