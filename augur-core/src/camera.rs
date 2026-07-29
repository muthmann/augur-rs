use crate::{config::CameraConfig, Result};

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub vendor: String,
    pub model: String,
    pub serial: Option<String>,
    pub firmware: Option<String>,
    pub compatible: Option<String>,
}

/// Absolute 8-bit bias DAC codes as they sit in the sensor's bias registers.
///
/// [`crate::config::BiasConfig`] only carries *relative* offsets, because the
/// usable code for a given bias is trimmed per sensor unit at the factory. The
/// code actually programmed is `factory_default + offset`, clamped to
/// `0..=255`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BiasCodes {
    pub diff_on: u8,
    pub diff_off: u8,
    pub fo: u8,
    pub hpf: u8,
    pub refr: u8,
}

/// Bias registers read back from the sensor together with the per-unit
/// factory defaults the configured offsets are relative to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BiasReadback {
    /// Codes currently programmed on the sensor.
    pub current: BiasCodes,
    /// Factory-trimmed defaults, i.e. the codes that correspond to offset 0.
    pub factory_default: BiasCodes,
}

/// Hardware-measured absolute values for settings the configuration can only
/// express in abstract or relative terms.
///
/// A `None` field means this sensor has no readback for that quantity — the
/// GUI must not substitute a computed guess, because no vendor-documented
/// mapping exists for the remaining biases.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SensorMonitoring {
    /// Measured pixel dead time, i.e. the refractory period: the minimum time
    /// between two events from one pixel. This is the absolute counterpart to
    /// the `refr` bias offset.
    pub pixel_dead_time_us: Option<f32>,
    /// Scene illumination as integrated by the sensor's LIFO block.
    pub illumination_lux: Option<f32>,
    /// Sensor die temperature.
    pub temperature_c: Option<f32>,
    /// Absolute bias codes plus the factory defaults they are offset from.
    pub biases: Option<BiasReadback>,
}

impl SensorMonitoring {
    /// True when no field carries a reading, i.e. the sensor reported nothing.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub trait EventCamera: Send {
    fn configure(&mut self, config: &CameraConfig) -> Result<()>;
    fn start_streaming(&mut self) -> Result<()>;
    fn stop_streaming(&mut self) -> Result<()>;
    fn device_info(&self) -> DeviceInfo;

    /// Reads the sensor's monitoring block for absolute counterparts to the
    /// abstract settings in [`CameraConfig`].
    ///
    /// Sources without monitoring hardware (replay, in-memory, ingress) keep
    /// the default and report nothing. Callers must treat this as a
    /// side-effecting device access: it issues register writes and reads, so
    /// it belongs on the same thread that owns camera control.
    fn read_monitoring(&mut self) -> Result<SensorMonitoring> {
        Ok(SensorMonitoring::default())
    }
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
