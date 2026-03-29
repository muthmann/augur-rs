use std::{
    io::Write,
    net::TcpStream,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use augur_core::pipeline::PreviewFrame;
use crossbeam_channel::{bounded, Sender};

use super::{ExternalTool, ExternalToolStatus};

pub const BUNDLED_IMAGEJ_PLUGIN_JAR: &[u8] = include_bytes!("../../imagej-plugin/AugurBridge.jar");
pub const BUNDLED_IMAGEJ_PLUGIN_JAR_NAME: &str = "AugurBridge.jar";
pub const DEFAULT_IMAGEJ_BRIDGE_PORT: u16 = 57_294;

#[derive(Debug)]
struct FrameEnvelope {
    frame: PreviewFrame,
    nm_per_pixel: f64,
}

pub struct ImageJBridge {
    host: String,
    port: u16,
    status: Arc<Mutex<ExternalToolStatus>>,
    frame_tx: Option<Sender<FrameEnvelope>>,
    handle: Option<JoinHandle<()>>,
}

impl ImageJBridge {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            status: Arc::new(Mutex::new(ExternalToolStatus::Disconnected)),
            frame_tx: None,
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
        if self.frame_tx.is_some() {
            return Ok(());
        }

        *self.status.lock().unwrap() = ExternalToolStatus::Connecting;

        let (tx, rx) = bounded::<FrameEnvelope>(2);
        let host = self.host.clone();
        let port = self.port;
        let status = Arc::clone(&self.status);
        let handle = thread::spawn(move || {
            let stream = TcpStream::connect((host.as_str(), port));
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(err) => {
                    *status.lock().unwrap() =
                        ExternalToolStatus::Error(format!("connect failed: {err}"));
                    return;
                }
            };
            let _ = stream.set_nodelay(true);
            *status.lock().unwrap() = ExternalToolStatus::Streaming;

            // Handshake: verify the bridge is alive via the text eval protocol.
            if let Err(err) = send_text_command(&mut stream, "eval return getVersion();") {
                *status.lock().unwrap() =
                    ExternalToolStatus::Error(format!("handshake failed: {err}"));
                return;
            }

            while let Ok(mut envelope) = rx.recv() {
                // Drop stale frames — always send the newest.
                while let Ok(next) = rx.try_recv() {
                    envelope = next;
                }

                if let Err(err) = send_frame(&mut stream, &envelope.frame, envelope.nm_per_pixel) {
                    *status.lock().unwrap() =
                        ExternalToolStatus::Error(format!("send frame failed: {err}"));
                    return;
                }
            }

            *status.lock().unwrap() = ExternalToolStatus::Disconnected;
        });

        self.frame_tx = Some(tx);
        self.handle = Some(handle);
        Ok(())
    }

    fn disconnect(&mut self) {
        self.frame_tx.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        *self.status.lock().unwrap() = ExternalToolStatus::Disconnected;
    }

    fn send_frame(&mut self, frame: &PreviewFrame, nm_per_pixel: f64) -> Result<(), String> {
        let Some(tx) = &self.frame_tx else {
            return Err("ImageJ bridge is not connected".into());
        };
        let envelope = FrameEnvelope {
            frame: frame.clone(),
            nm_per_pixel,
        };
        let _ = tx.try_send(envelope);
        Ok(())
    }
}

/// Send a binary frame to the AugurBridge plugin.
///
/// Protocol: `frame <width> <height> <nm_per_pixel>\n` followed by
/// `width * height * 2` bytes of raw 16-bit little-endian pixel data.
fn send_frame(
    stream: &mut TcpStream,
    frame: &PreviewFrame,
    nm_per_pixel: f64,
) -> Result<(), String> {
    let header = format!("frame {} {} {nm_per_pixel}\n", frame.width, frame.height);
    stream
        .write_all(header.as_bytes())
        .map_err(|err| format!("header write failed: {err}"))?;

    // Transmit pixels as raw little-endian u16 bytes.
    let byte_len = frame.pixels.len() * 2;
    let mut buf = Vec::with_capacity(byte_len);
    for &px in &frame.pixels {
        buf.extend_from_slice(&px.to_le_bytes());
    }
    stream
        .write_all(&buf)
        .map_err(|err| format!("pixel write failed: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("flush failed: {err}"))?;
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
