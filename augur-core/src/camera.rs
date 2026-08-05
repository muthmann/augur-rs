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

/// Selects which monitoring blocks a camera should physically read.
///
/// Keeping this separate from [`SensorMonitoring`] lets low-rate recorders
/// avoid waking unrelated ADC/register blocks merely to discard their values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SensorMonitoringSelection {
    pub pixel_dead_time: bool,
    pub illumination: bool,
    pub temperature: bool,
    pub biases: bool,
}

impl SensorMonitoringSelection {
    pub const ALL: Self = Self {
        pixel_dead_time: true,
        illumination: true,
        temperature: true,
        biases: true,
    };

    pub const ILLUMINATION: Self = Self {
        illumination: true,
        ..Self::NONE
    };

    pub const SLOW_TELEMETRY: Self = Self {
        pixel_dead_time: true,
        temperature: true,
        ..Self::NONE
    };

    pub const NONE: Self = Self {
        pixel_dead_time: false,
        illumination: false,
        temperature: false,
        biases: false,
    };

    pub fn union(self, other: Self) -> Self {
        Self {
            pixel_dead_time: self.pixel_dead_time || other.pixel_dead_time,
            illumination: self.illumination || other.illumination,
            temperature: self.temperature || other.temperature,
            biases: self.biases || other.biases,
        }
    }

    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    fn filter(self, mut values: SensorMonitoring) -> SensorMonitoring {
        if !self.pixel_dead_time {
            values.pixel_dead_time_us = None;
        }
        if !self.illumination {
            values.illumination_lux = None;
        }
        if !self.temperature {
            values.temperature_c = None;
        }
        if !self.biases {
            values.biases = None;
        }
        values
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

    /// Reads only the requested monitoring blocks when the hardware supports
    /// selective access. The default preserves compatibility with camera
    /// implementations that can only perform one monolithic read.
    fn read_monitoring_selected(
        &mut self,
        selection: SensorMonitoringSelection,
    ) -> Result<SensorMonitoring> {
        self.read_monitoring()
            .map(|values| selection.filter(values))
    }
}

/// Stream-only packet reader split off a `PacketStreamCamera`, so the
/// pipeline can keep draining the device on a dedicated thread while camera
/// control (configure, start/stop) runs elsewhere.
pub trait PacketStreamReader: Send {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Services the transport without delivering a packet.
    ///
    /// Readers that keep transfers queued in the kernel must be able to reap
    /// completions and re-arm them even while the pipeline has no free buffer
    /// to receive data into. Without this the transfer queue drains during a
    /// downstream stall, the device endpoint runs dry, and the camera FIFO
    /// overflows — a hard, unrecoverable gap in the recording that is far
    /// longer than the stall itself.
    ///
    /// Implementations must return within roughly `budget` and must never
    /// discard received data. The default is a no-op, which is correct for
    /// readers that hold no queued transfers.
    fn service(&mut self, _budget: std::time::Duration) {}

    /// Moves one packet the reader already received, but has not delivered,
    /// into `out`.
    ///
    /// Called after the stream loop stops so data the device already handed to
    /// the host still reaches the recording instead of being dropped with the
    /// reader. Returns `Ok(0)` when nothing is buffered. The default reports
    /// no buffered data.
    fn take_buffered_packet(&mut self, _out: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
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
