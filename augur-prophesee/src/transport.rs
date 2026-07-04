use std::{sync::Arc, time::Duration};

use augur_core::{CameraError, Result};
use rusb::{Context, Device, DeviceHandle, Direction, TransferType, UsbContext};

const TREUZELL_USB_CLASS: u8 = 0xff;
const TREUZELL_SUBCLASS: u8 = 0x19;
const TREUZELL_PROTOCOL: u8 = 0x00;

const VID_PID_CANDIDATES: &[(u16, u16)] = &[
    (0x03fd, 0x5832),
    (0x04b4, 0x00f4),
    (0x04b4, 0x00f5),
    (0x1fc9, 0x5838),
];

#[derive(Debug, Clone, Default)]
pub struct BoardInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub usb_serial: Option<String>,
}

struct TransportShared {
    _ctx: Context,
    handle: DeviceHandle<Context>,
}

/// USB transport for a Treuzell board.
///
/// The device handle is shared, so a `Transport` can be cloned to move stream
/// reads onto a dedicated thread while control transfers continue elsewhere.
/// libusb supports concurrent synchronous transfers on different endpoints
/// from different threads; callers must keep control-command sequencing
/// (write + read pairs) on a single thread, which `Treuzell`'s `&mut`
/// borrow of one clone enforces.
#[derive(Clone)]
pub struct Transport {
    shared: Arc<TransportShared>,
    interface: u8,
    ep_ctrl_in: u8,
    ep_ctrl_out: u8,
    ep_stream_in: u8,
    control_timeout: Duration,
    stream_timeout: Duration,
    board_info: BoardInfo,
}

impl Transport {
    pub fn open_first() -> Result<Self> {
        let ctx = Context::new().map_err(|e| CameraError::Transport(e.to_string()))?;
        let devices = ctx
            .devices()
            .map_err(|e| CameraError::Transport(e.to_string()))?;

        for device in devices.iter() {
            let descriptor = match device.device_descriptor() {
                Ok(d) => d,
                Err(_) => continue,
            };

            if !VID_PID_CANDIDATES.contains(&(descriptor.vendor_id(), descriptor.product_id())) {
                continue;
            }

            let Some((iface, ep_ctrl_in, ep_ctrl_out, ep_stream_in)) =
                find_treuzell_interface(&device)
            else {
                continue;
            };

            let handle = match device.open() {
                Ok(h) => h,
                Err(_) => continue,
            };

            let _ = handle.set_auto_detach_kernel_driver(true);
            if handle.claim_interface(iface).is_err() {
                continue;
            }
            let _ = handle.set_alternate_setting(iface, 0);

            let board_info = BoardInfo {
                vendor_id: descriptor.vendor_id(),
                product_id: descriptor.product_id(),
                manufacturer: read_descriptor_string(
                    &handle,
                    descriptor.manufacturer_string_index(),
                ),
                product: read_descriptor_string(&handle, descriptor.product_string_index()),
                usb_serial: read_descriptor_string(
                    &handle,
                    descriptor.serial_number_string_index(),
                ),
            };

            return Ok(Self {
                shared: Arc::new(TransportShared { _ctx: ctx, handle }),
                interface: iface,
                ep_ctrl_in,
                ep_ctrl_out,
                ep_stream_in,
                control_timeout: Duration::from_millis(1_000),
                stream_timeout: Duration::from_millis(100),
                board_info,
            });
        }

        Err(CameraError::Transport(
            "no Treuzell-compatible EVK device found (VID:PID 04b4:00f4/00f5 etc)".into(),
        ))
    }

    pub fn board_info(&self) -> &BoardInfo {
        &self.board_info
    }

    pub(crate) fn raw_context(&self) -> *mut rusb::ffi::libusb_context {
        self.shared._ctx.as_raw()
    }

    pub(crate) fn raw_handle(&self) -> *mut rusb::ffi::libusb_device_handle {
        self.shared.handle.as_raw()
    }

    pub(crate) fn stream_endpoint(&self) -> u8 {
        self.ep_stream_in
    }

    pub(crate) fn stream_timeout(&self) -> Duration {
        self.stream_timeout
    }

    pub fn interface_number(&self) -> u8 {
        self.interface
    }

    pub fn write_control(&mut self, data: &[u8]) -> Result<usize> {
        self.shared
            .handle
            .write_bulk(self.ep_ctrl_out, data, self.control_timeout)
            .map_err(|e| CameraError::Transport(e.to_string()))
    }

    pub fn read_control(&mut self, out: &mut [u8]) -> Result<usize> {
        self.shared
            .handle
            .read_bulk(self.ep_ctrl_in, out, self.control_timeout)
            .map_err(|e| CameraError::Transport(e.to_string()))
    }

    pub fn read_stream(&mut self, out: &mut [u8]) -> Result<usize> {
        self.shared
            .handle
            .read_bulk(self.ep_stream_in, out, self.stream_timeout)
            .map_err(|e| match e {
                rusb::Error::Timeout => CameraError::Timeout("USB stream read timed out".into()),
                _ => CameraError::Transport(e.to_string()),
            })
    }
}

fn find_treuzell_interface(device: &Device<Context>) -> Option<(u8, u8, u8, u8)> {
    let config = device.active_config_descriptor().ok()?;

    for interface in config.interfaces() {
        for interface_desc in interface.descriptors() {
            if interface_desc.class_code() != TREUZELL_USB_CLASS
                || interface_desc.sub_class_code() != TREUZELL_SUBCLASS
                || interface_desc.protocol_code() != TREUZELL_PROTOCOL
            {
                continue;
            }

            if interface_desc.num_endpoints() < 3 {
                continue;
            }

            let endpoints: Vec<_> = interface_desc.endpoint_descriptors().collect();
            if endpoints.len() < 3 {
                continue;
            }

            // OpenEB expects endpoint order: bulk IN control, bulk OUT control, bulk IN stream.
            if endpoints[0].transfer_type() != TransferType::Bulk
                || endpoints[0].direction() != Direction::In
                || endpoints[1].transfer_type() != TransferType::Bulk
                || endpoints[1].direction() != Direction::Out
                || endpoints[2].transfer_type() != TransferType::Bulk
                || endpoints[2].direction() != Direction::In
            {
                continue;
            }

            return Some((
                interface_desc.interface_number(),
                endpoints[0].address(),
                endpoints[1].address(),
                endpoints[2].address(),
            ));
        }
    }

    None
}

fn read_descriptor_string(handle: &DeviceHandle<Context>, idx: Option<u8>) -> Option<String> {
    idx.and_then(|i| handle.read_string_descriptor_ascii(i).ok())
}
