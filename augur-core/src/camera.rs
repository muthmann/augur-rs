use crate::{config::CameraConfig, Result};

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub vendor: String,
    pub model: String,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub compatible: Option<String>,
}

pub trait EventCamera: Send {
    fn configure(&mut self, config: &CameraConfig) -> Result<()>;
    fn start_streaming(&mut self) -> Result<()>;
    fn stop_streaming(&mut self) -> Result<()>;
    fn device_info(&self) -> DeviceInfo;
}

/// Stream-only packet reader split off a `PacketStreamCamera`, so the
/// pipeline can keep draining the device on a dedicated thread while camera
/// control (configure, start/stop) runs elsewhere.
pub trait PacketStreamReader: Send {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize>;
}

pub trait PacketStreamCamera: EventCamera {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Splits off an independent stream reader sharing the underlying
    /// transport. When supported, the pipeline reads packets on a dedicated
    /// thread while reconfiguration runs on a control thread, so applying
    /// settings never stalls stream reads (a stalled reader overflows the
    /// camera FIFO and leaves gaps in the recording).
    fn split_stream_reader(&mut self) -> Option<Box<dyn PacketStreamReader>> {
        None
    }
}
