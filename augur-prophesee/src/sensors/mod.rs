mod imx636;

use augur_core::{config::*, Result};

pub use imx636::Imx636;

use crate::transport::Transport;

pub trait PseeSensor: Send {
    fn name(&self) -> &'static str;
    fn geometry(&self) -> (u16, u16);

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
}
