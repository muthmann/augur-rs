mod imx636;

use augur_core::{
    camera::{SensorMonitoring, SensorMonitoringSelection},
    config::*,
    Result,
};

pub use imx636::Imx636;

use crate::transport::Transport;

pub trait PseeSensor: Send {
    fn name(&self) -> &'static str;
    fn geometry(&self) -> (u16, u16);

    /// Validate and freeze a complete configuration before any register is
    /// changed. Implementations may resolve device-specific resources such as
    /// file-backed masks into the returned configuration.
    fn prepare_configuration(&self, config: &CameraConfig) -> Result<CameraConfig> {
        let (width, height) = self.geometry();
        config.validate(width, height)?;
        Ok(config.clone())
    }

    fn init(&mut self, transport: &mut Transport) -> Result<()>;
    fn set_biases(&mut self, transport: &mut Transport, cfg: &BiasConfig) -> Result<()>;
    fn set_roi(&mut self, transport: &mut Transport, roi: &RoiConfig) -> Result<()>;
    fn set_pixel_mask(&mut self, transport: &mut Transport, mask: &PixelMaskConfig) -> Result<()>;
    fn set_digital_filter(
        &mut self,
        transport: &mut Transport,
        filter: &DigitalFilterConfig,
    ) -> Result<()>;
    fn set_external_trigger(
        &mut self,
        _transport: &mut Transport,
        cfg: &ExternalTriggerConfig,
    ) -> Result<()> {
        if cfg.enabled {
            return Err(augur_core::CameraError::Config(
                "this sensor does not support external trigger input".into(),
            ));
        }
        Ok(())
    }
    fn start_streaming(&mut self, transport: &mut Transport) -> Result<()>;
    fn stop_streaming(&mut self, transport: &mut Transport) -> Result<()>;

    /// Reads absolute values for settings the config only expresses as
    /// relative offsets. Sensors without a monitoring block report nothing.
    fn read_monitoring(&mut self, _transport: &mut Transport) -> Result<SensorMonitoring> {
        Ok(SensorMonitoring::default())
    }

    fn read_monitoring_selected(
        &mut self,
        transport: &mut Transport,
        _selection: SensorMonitoringSelection,
    ) -> Result<SensorMonitoring> {
        self.read_monitoring(transport)
    }
}
