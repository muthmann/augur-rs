use augur_core::{
    camera::{DeviceInfo, EventCamera, PacketStreamCamera},
    config::CameraConfig,
    CameraError, Result,
};

use crate::{
    debug,
    sensors::{Imx636, PseeSensor},
    transport::Transport,
    treuzell::Treuzell,
};

pub struct Evk4Camera<S: PseeSensor> {
    transport: Transport,
    sensor: S,
    config: CameraConfig,
    info: DeviceInfo,
    streaming: bool,
}

impl Evk4Camera<Imx636> {
    pub fn open_imx636() -> Result<Self> {
        let mut transport = Transport::open_first()?;
        let board = transport.board_info().clone();
        let interface_number = transport.interface_number();

        let mut fw_version = None;
        let mut serial = None;
        let mut sensor_id = None;
        let mut sensor_compatible = Vec::new();
        let mut sensor_name = None;
        let mut discovered_devices = Vec::new();

        {
            let mut tz = Treuzell::new(&mut transport);

            if let Ok(v) = tz.release_version() {
                fw_version = Some(format!("0x{v:06x}"));
            }
            if let Ok(raw) = tz.serial_raw() {
                serial = format_serial_hex(&raw);
            }

            let device_count = tz.get_device_count()?;
            debug::log(format!(
                "Treuzell reported {device_count} device(s) on interface {}",
                interface_number
            ));
            for dev in 0..device_count {
                let device_name = tz.get_device_name(dev);
                let compatible_result = tz.get_device_compatible(dev);
                let compatible = compatible_result.as_ref().cloned().unwrap_or_default();
                let probe_result = Imx636::probe(&mut tz, dev);
                let has_imx636_compat = compatible.iter().any(|c| {
                    c.contains("imx636") || c.contains("ccam5_gen42") || c.contains("gen42")
                });
                let probe_match = probe_result.as_ref().copied().unwrap_or(false);

                let compatible_summary = if compatible.is_empty() {
                    match &compatible_result {
                        Ok(_) => "<none>".to_string(),
                        Err(err) => format!("<error: {err}>"),
                    }
                } else {
                    compatible.join(",")
                };
                let name_summary = match &device_name {
                    Ok(name) => name.clone(),
                    Err(err) => format!("<error: {err}>"),
                };
                let probe_summary = match &probe_result {
                    Ok(value) => format!("{value}"),
                    Err(err) => format!("error: {err}"),
                };
                let summary = format!(
                    "device {dev}: name={name_summary}, compatible={compatible_summary}, imx636_compat={has_imx636_compat}, probe={probe_summary}"
                );
                debug::log(&summary);
                discovered_devices.push(summary);

                if has_imx636_compat || probe_match {
                    sensor_name = device_name.ok();
                    sensor_id = Some(dev);
                    sensor_compatible = compatible;
                    debug::log(format!("selected Treuzell device {dev} for IMX636 init"));
                    break;
                }
            }
        }

        let sensor_id = sensor_id.ok_or_else(|| {
            CameraError::Transport(format!(
                "no IMX636-compatible Treuzell device found on board; discovered: {}",
                if discovered_devices.is_empty() {
                    "<none>".to_string()
                } else {
                    discovered_devices.join(" | ")
                }
            ))
        })?;

        let mut sensor = Imx636::new(sensor_id, sensor_compatible.clone());
        sensor.init(&mut transport)?;

        let info = DeviceInfo {
            vendor: board.manufacturer.unwrap_or_else(|| "Prophesee".into()),
            model: board.product.unwrap_or_else(|| "EVK4".into()),
            serial: serial.or(board.usb_serial),
            firmware: fw_version,
            compatible: Some(if sensor_compatible.is_empty() {
                sensor_name.unwrap_or_else(|| sensor.name().into())
            } else {
                sensor_compatible.join(",")
            }),
        };

        Ok(Self {
            transport,
            sensor,
            config: CameraConfig::default(),
            info,
            streaming: false,
        })
    }
}

impl<S: PseeSensor> EventCamera for Evk4Camera<S> {
    fn configure(&mut self, config: &CameraConfig) -> Result<()> {
        let (w, h) = self.sensor.geometry();
        config.validate(w, h)?;
        self.sensor
            .set_biases(&mut self.transport, &config.biases)?;
        self.sensor.set_roi(&mut self.transport, &config.roi)?;
        self.sensor
            .set_pixel_mask(&mut self.transport, &config.pixel_mask)?;
        self.sensor
            .set_digital_filter(&mut self.transport, &config.digital_filter)?;
        self.config = config.clone();
        Ok(())
    }

    fn start_streaming(&mut self) -> Result<()> {
        if !self.streaming {
            self.sensor.start_streaming(&mut self.transport)?;
            self.streaming = true;
        }
        Ok(())
    }

    fn stop_streaming(&mut self) -> Result<()> {
        if self.streaming {
            self.sensor.stop_streaming(&mut self.transport)?;
            self.streaming = false;
        }
        Ok(())
    }

    fn device_info(&self) -> DeviceInfo {
        self.info.clone()
    }
}

impl<S: PseeSensor> PacketStreamCamera for Evk4Camera<S> {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.transport.read_stream(buf)
    }
}

fn format_serial_hex(raw: &[u8]) -> Option<String> {
    if raw.len() >= 8 {
        let v = u64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]);
        return Some(format!("{v:016x}"));
    }
    if raw.len() >= 4 {
        let v = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
        return Some(format!("{v:08x}"));
    }
    None
}
