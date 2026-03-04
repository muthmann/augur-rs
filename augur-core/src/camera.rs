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

pub trait PacketStreamCamera: EventCamera {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize>;
}
