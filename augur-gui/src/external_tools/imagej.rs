use std::{
    io::Write,
    net::TcpStream,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use augur_core::pipeline::PreviewFrame;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};

use super::{ExternalTool, ExternalToolStatus};

pub const BUNDLED_IMAGEJ_PLUGIN_JAR: &[u8] = include_bytes!("../../imagej-plugin/AugurBridge.jar");
pub const BUNDLED_IMAGEJ_PLUGIN_JAR_NAME: &str = "AugurBridge.jar";
pub const DEFAULT_IMAGEJ_BRIDGE_PORT: u16 = 57_294;

#[derive(Debug)]
struct FrameEnvelope {
    width: u16,
    height: u16,
    pixels: Vec<u16>,
    nm_per_pixel: f64,
}

impl FrameEnvelope {
    fn from_preview_frame(frame: &PreviewFrame, nm_per_pixel: f64) -> Self {
        Self {
            width: frame.width,
            height: frame.height,
            pixels: frame.pixels.clone(),
            nm_per_pixel,
        }
    }
}

pub struct ImageJBridge {
    host: String,
    port: u16,
    status: Arc<Mutex<ExternalToolStatus>>,
    wake_tx: Option<Sender<()>>,
    pending_frame: Arc<Mutex<Option<FrameEnvelope>>>,
    handle: Option<JoinHandle<()>>,
}

impl ImageJBridge {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            status: Arc::new(Mutex::new(ExternalToolStatus::Disconnected)),
            wake_tx: None,
            pending_frame: Arc::new(Mutex::new(None)),
            handle: None,
        }
    }
}

impl ExternalTool for ImageJBridge {
    fn name(&self) -> &str {
        "ImageJ"
    }

    fn status(&self) -> ExternalToolStatus {
        self.status.lock().unwrap().clone()
    }

    fn connect(&mut self) -> Result<(), String> {
        if self.wake_tx.is_some() {
            return Ok(());
        }

        *self.status.lock().unwrap() = ExternalToolStatus::Connecting;

        *self.pending_frame.lock().unwrap() = None;

        let (tx, rx) = bounded::<()>(1);
        let host = self.host.clone();
        let port = self.port;
        let status = Arc::clone(&self.status);
        let pending_frame = Arc::clone(&self.pending_frame);
        let handle = thread::spawn(move || {
            run_bridge_worker(host, port, status, pending_frame, rx);
        });

        self.wake_tx = Some(tx);
        self.handle = Some(handle);
        Ok(())
    }

    fn disconnect(&mut self) {
        self.wake_tx.take();
        *self.pending_frame.lock().unwrap() = None;
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        *self.status.lock().unwrap() = ExternalToolStatus::Disconnected;
    }

    fn send_frame(&mut self, frame: &PreviewFrame, nm_per_pixel: f64) -> Result<(), String> {
        let Some(tx) = &self.wake_tx else {
            return Err("ImageJ bridge is not connected".into());
        };

        *self.pending_frame.lock().unwrap() =
            Some(FrameEnvelope::from_preview_frame(frame, nm_per_pixel));
        match tx.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err("ImageJ bridge is not connected".into()),
        }
    }
}

fn run_bridge_worker(
    host: String,
    port: u16,
    status: Arc<Mutex<ExternalToolStatus>>,
    pending_frame: Arc<Mutex<Option<FrameEnvelope>>>,
    wake_rx: Receiver<()>,
) {
    let stream = TcpStream::connect((host.as_str(), port));
    let mut stream = match stream {
        Ok(stream) => stream,
        Err(err) => {
            *status.lock().unwrap() = ExternalToolStatus::Error(format!("connect failed: {err}"));
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    *status.lock().unwrap() = ExternalToolStatus::Streaming;

    // Handshake: verify the bridge is alive via the text eval protocol.
    if let Err(err) = send_text_command(&mut stream, "eval return getVersion();") {
        *status.lock().unwrap() = ExternalToolStatus::Error(format!("handshake failed: {err}"));
        return;
    }

    while wake_rx.recv().is_ok() {
        let Some(envelope) = take_latest_pending_frame(&pending_frame, &wake_rx) else {
            continue;
        };

        if let Err(err) = write_frame_packet(&mut stream, &envelope) {
            *status.lock().unwrap() =
                ExternalToolStatus::Error(format!("send frame failed: {err}"));
            return;
        }
    }

    *status.lock().unwrap() = ExternalToolStatus::Disconnected;
}

fn take_latest_pending_frame(
    pending_frame: &Arc<Mutex<Option<FrameEnvelope>>>,
    wake_rx: &Receiver<()>,
) -> Option<FrameEnvelope> {
    let mut latest = pending_frame.lock().unwrap().take();
    while wake_rx.try_recv().is_ok() {
        let next = pending_frame.lock().unwrap().take();
        if next.is_some() {
            latest = next;
        }
    }
    latest
}

/// Send a binary frame to the AugurBridge plugin.
///
/// Protocol: `frame <width> <height> <nm_per_pixel>\n` followed by
/// `width * height * 2` bytes of raw 16-bit little-endian pixel data.
fn write_frame_packet<W: Write>(writer: &mut W, frame: &FrameEnvelope) -> Result<(), String> {
    writeln!(
        writer,
        "frame {} {} {}",
        frame.width, frame.height, frame.nm_per_pixel
    )
    .map_err(|err| format!("header write failed: {err}"))?;
    write_pixels(writer, &frame.pixels)?;
    writer
        .flush()
        .map_err(|err| format!("flush failed: {err}"))?;
    Ok(())
}

fn write_pixels<W: Write>(writer: &mut W, pixels: &[u16]) -> Result<(), String> {
    const PIXELS_PER_CHUNK: usize = 4096;
    let mut buf = [0u8; PIXELS_PER_CHUNK * 2];

    for chunk in pixels.chunks(PIXELS_PER_CHUNK) {
        for (dst, &pixel) in buf[..chunk.len() * 2].chunks_exact_mut(2).zip(chunk) {
            dst.copy_from_slice(&pixel.to_le_bytes());
        }
        writer
            .write_all(&buf[..chunk.len() * 2])
            .map_err(|err| format!("pixel write failed: {err}"))?;
    }

    Ok(())
}

/// Send a text-protocol command (e.g. `eval <macro>`).
fn send_text_command(stream: &mut TcpStream, command: &str) -> Result<(), String> {
    let line = format!("{command}\n");
    stream
        .write_all(line.as_bytes())
        .map_err(|err| format!("socket write failed: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("socket flush failed: {err}"))?;
    Ok(())
}

impl Drop for ImageJBridge {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[cfg(test)]
mod tests {
    use super::{write_frame_packet, FrameEnvelope};

    #[test]
    fn frame_packet_writes_header_and_little_endian_pixels() {
        let frame = FrameEnvelope {
            width: 2,
            height: 1,
            pixels: vec![1, 0x0203],
            nm_per_pixel: 42.5,
        };
        let mut out = Vec::new();

        write_frame_packet(&mut out, &frame).expect("frame packet should encode");

        let expected_prefix = b"frame 2 1 42.5\n";
        assert_eq!(&out[..expected_prefix.len()], expected_prefix);
        assert_eq!(&out[expected_prefix.len()..], &[1, 0, 3, 2]);
    }
}
