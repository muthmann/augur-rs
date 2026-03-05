use augur_core::{CameraError, Result};

use crate::transport::Transport;

pub const TZ_FAILURE_FLAG: u32 = 0x8000_0000;
pub const TZ_WRITE_FLAG: u32 = 0x4000_0000;
pub const TZ_UNKNOWN_CMD: u32 = TZ_FAILURE_FLAG;

pub const TZ_PROP_SERIAL: u32 = 0x72;
pub const TZ_PROP_RELEASE_VERSION: u32 = 0x79;
pub const TZ_PROP_BUILD_DATE: u32 = 0x7a;

pub const TZ_PROP_DEVICES: u32 = 0x10000;
pub const TZ_PROP_DEVICE_NAME: u32 = 0x10001;
pub const TZ_PROP_DEVICE_COMPATIBLE: u32 = 0x10003;
pub const TZ_PROP_DEVICE_ENABLE: u32 = 0x10010;
pub const TZ_PROP_DEVICE_REG32: u32 = 0x10102;
pub const TZ_PROP_DEVICE_STREAM: u32 = 0x10200;
pub const TZ_PROP_DEVICE_OUTPUT_FORMAT: u32 = 0x10201;

pub struct Treuzell<'a> {
    transport: &'a mut Transport,
}

impl<'a> Treuzell<'a> {
    pub fn new(transport: &'a mut Transport) -> Self {
        Self { transport }
    }

    pub fn release_version(&mut self) -> Result<u32> {
        let resp = self.transact(TZ_PROP_RELEASE_VERSION, &[], false)?;
        read_u32(&resp.payload, 0)
    }

    pub fn build_date(&mut self) -> Result<u64> {
        let resp = self.transact(TZ_PROP_BUILD_DATE, &[], false)?;
        let bytes: [u8; 8] = resp
            .payload
            .get(..8)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(|| {
                CameraError::Transport("Treuzell build-date response too short".into())
            })?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn serial_raw(&mut self) -> Result<Vec<u8>> {
        let resp = self.transact(TZ_PROP_SERIAL, &[], false)?;
        Ok(resp.payload)
    }

    pub fn get_device_count(&mut self) -> Result<u32> {
        let resp = self.transact(TZ_PROP_DEVICES, &[], false)?;
        read_u32(&resp.payload, 0)
    }

    pub fn get_device_name(&mut self, device_id: u32) -> Result<String> {
        let resp = self.transact(TZ_PROP_DEVICE_NAME, &device_id.to_le_bytes(), false)?;
        let strings = parse_device_string_list(&resp.payload, device_id)?;
        strings
            .into_iter()
            .next()
            .ok_or_else(|| CameraError::Transport("Treuzell device-name response empty".into()))
    }

    pub fn get_device_compatible(&mut self, device_id: u32) -> Result<Vec<String>> {
        let resp = self.transact(TZ_PROP_DEVICE_COMPATIBLE, &device_id.to_le_bytes(), false)?;
        parse_device_string_list(&resp.payload, device_id)
    }

    pub fn device_enable(&mut self, device_id: u32, on: bool) -> Result<()> {
        let payload = u32_pair_payload(device_id, on as u32);
        self.transact(TZ_PROP_DEVICE_ENABLE, &payload, true)
            .map(|_| ())
    }

    pub fn device_stream(&mut self, device_id: u32, on: bool) -> Result<()> {
        let payload = u32_pair_payload(device_id, on as u32);
        self.transact(TZ_PROP_DEVICE_STREAM, &payload, true)
            .map(|_| ())
    }

    pub fn set_output_format(&mut self, device_id: u32, format: &str) -> Result<String> {
        let mut payload = Vec::with_capacity(4 + format.len() + 1);
        payload.extend_from_slice(&device_id.to_le_bytes());
        payload.extend_from_slice(format.as_bytes());
        payload.push(0);
        let resp = self.transact(TZ_PROP_DEVICE_OUTPUT_FORMAT, &payload, true)?;
        let strings = parse_device_string_list(&resp.payload, device_id)?;
        strings
            .into_iter()
            .next()
            .ok_or_else(|| CameraError::Transport("Treuzell set-format response empty".into()))
    }

    pub fn read_device_register(
        &mut self,
        device_id: u32,
        addr: u32,
        n_values: u32,
    ) -> Result<Vec<u32>> {
        let mut payload = [0u8; 12];
        payload[..4].copy_from_slice(&device_id.to_le_bytes());
        payload[4..8].copy_from_slice(&addr.to_le_bytes());
        payload[8..].copy_from_slice(&n_values.to_le_bytes());

        let resp = self.transact(TZ_PROP_DEVICE_REG32, &payload, false)?;

        let got_dev = read_u32(&resp.payload, 0)?;
        let got_addr = read_u32(&resp.payload, 1)?;
        if got_dev != device_id || got_addr != addr {
            return Err(CameraError::Transport(format!(
                "Treuzell reg32 read mismatch (req dev={device_id} addr=0x{addr:08x}, got dev={got_dev} addr=0x{got_addr:08x})"
            )));
        }

        let expected_words = (n_values as usize) + 2;
        if resp.payload.len() < expected_words * 4 {
            return Err(CameraError::Transport(
                "Treuzell reg32 read response too short".into(),
            ));
        }

        (0..n_values as usize)
            .map(|i| read_u32(&resp.payload, i + 2))
            .collect()
    }

    pub fn write_device_register(
        &mut self,
        device_id: u32,
        addr: u32,
        values: &[u32],
    ) -> Result<()> {
        let mut payload = Vec::with_capacity((values.len() + 2) * 4);
        payload.extend_from_slice(&device_id.to_le_bytes());
        payload.extend_from_slice(&addr.to_le_bytes());
        for v in values {
            payload.extend_from_slice(&v.to_le_bytes());
        }

        let resp = self.transact(TZ_PROP_DEVICE_REG32, &payload, true)?;
        let got_dev = read_u32(&resp.payload, 0)?;
        let got_addr = read_u32(&resp.payload, 1)?;
        if got_dev != device_id || got_addr != addr {
            return Err(CameraError::Transport(format!(
                "Treuzell reg32 write mismatch (req dev={device_id} addr=0x{addr:08x}, got dev={got_dev} addr=0x{got_addr:08x})"
            )));
        }
        Ok(())
    }

    pub fn read_reg32(&mut self, device_id: u32, addr: u32) -> Result<u32> {
        let mut vals = self.read_device_register(device_id, addr, 1)?;
        vals.pop()
            .ok_or_else(|| CameraError::Transport("reg32 read returned no value".into()))
    }

    pub fn write_reg32(&mut self, device_id: u32, addr: u32, value: u32) -> Result<()> {
        self.write_device_register(device_id, addr, &[value])
    }

    fn transact(&mut self, property: u32, payload: &[u8], write: bool) -> Result<Response> {
        let req_property = if write {
            property | TZ_WRITE_FLAG
        } else {
            property
        };

        let mut request = Vec::with_capacity(8 + payload.len());
        request.extend_from_slice(&req_property.to_le_bytes());
        request.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        request.extend_from_slice(payload);

        self.transport.write_control(&request)?;

        let mut response_buf = vec![0_u8; 16 * 1024];
        let n = self.transport.read_control(&mut response_buf)?;
        if n < 8 {
            return Err(CameraError::Transport(
                "Treuzell response shorter than header".into(),
            ));
        }

        let property = u32::from_le_bytes(response_buf[0..4].try_into().unwrap());
        let size = u32::from_le_bytes(response_buf[4..8].try_into().unwrap()) as usize;

        if size != n - 8 {
            return Err(CameraError::Transport(format!(
                "Treuzell size mismatch (header={size}, frame={})",
                n - 8
            )));
        }

        if property == TZ_UNKNOWN_CMD {
            return Err(CameraError::Transport(
                "Treuzell command not implemented by device".into(),
            ));
        }
        if property == (req_property | TZ_FAILURE_FLAG) {
            return Err(CameraError::Transport(format!(
                "Treuzell command failed for property 0x{req_property:08x}"
            )));
        }
        if property != req_property {
            return Err(CameraError::Transport(format!(
                "Treuzell property mismatch (req=0x{req_property:08x}, resp=0x{property:08x})"
            )));
        }

        Ok(Response {
            payload: response_buf[8..(8 + size)].to_vec(),
        })
    }
}

#[derive(Debug, Clone)]
struct Response {
    payload: Vec<u8>,
}

fn u32_pair_payload(a: u32, b: u32) -> [u8; 8] {
    let mut buf = [0u8; 8];
    buf[..4].copy_from_slice(&a.to_le_bytes());
    buf[4..].copy_from_slice(&b.to_le_bytes());
    buf
}

fn read_u32(payload: &[u8], index: usize) -> Result<u32> {
    let start = index * 4;
    let bytes: [u8; 4] = payload
        .get(start..start + 4)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| {
            CameraError::Transport(format!("Treuzell payload too short for u32 index {index}"))
        })?;
    Ok(u32::from_le_bytes(bytes))
}

fn parse_device_string_list(payload: &[u8], expected_device: u32) -> Result<Vec<String>> {
    if payload.len() < 5 {
        return Err(CameraError::Transport(
            "Treuzell device-string response too short".into(),
        ));
    }

    let dev = read_u32(payload, 0)?;
    if dev != expected_device {
        return Err(CameraError::Transport(format!(
            "Treuzell device-string response device mismatch (expected {expected_device}, got {dev})"
        )));
    }

    let strings_blob = &payload[4..];
    if strings_blob.last().copied() != Some(0) {
        return Err(CameraError::Transport(
            "Treuzell string list is not NULL terminated".into(),
        ));
    }

    let mut strings = Vec::new();
    for part in strings_blob.split(|b| *b == 0) {
        if part.is_empty() {
            continue;
        }
        let s = String::from_utf8(part.to_vec()).map_err(|e| {
            CameraError::Transport(format!("invalid UTF-8 in Treuzell string: {e}"))
        })?;
        strings.push(s);
    }

    if strings.is_empty() {
        return Err(CameraError::Transport(
            "Treuzell string list returned no strings".into(),
        ));
    }

    Ok(strings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_device_string_payload() {
        let payload = [
            0x02, 0x00, 0x00, 0x00, b'p', b's', b'e', b'e', b',', b'i', b'm', b'x', b'6', b'3',
            b'6', 0x00, b'p', b's', b'e', b'e', b',', b'g', b'e', b'n', b'4', b'2', 0x00,
        ];
        let strings = parse_device_string_list(&payload, 2).expect("should parse");
        assert_eq!(strings.len(), 2);
        assert_eq!(strings[0], "psee,imx636");
        assert_eq!(strings[1], "psee,gen42");
    }

    #[test]
    fn rejects_non_terminated_string_payload() {
        let payload = [0x00, 0x00, 0x00, 0x00, b'x', b'y', b'z'];
        let err = parse_device_string_list(&payload, 0).expect_err("should fail");
        assert!(err.to_string().contains("NULL terminated"));
    }

    #[test]
    fn reads_u32_little_endian() {
        let payload = [0x78, 0x56, 0x34, 0x12, 0xff, 0x00, 0x00, 0x00];
        assert_eq!(read_u32(&payload, 0).unwrap(), 0x1234_5678);
        assert_eq!(read_u32(&payload, 1).unwrap(), 0x0000_00ff);
    }
}
