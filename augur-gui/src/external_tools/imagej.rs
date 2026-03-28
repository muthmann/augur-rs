use std::{
    fs,
    io::Write,
    net::TcpStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
};

use augur_core::pipeline::PreviewFrame;
use crossbeam_channel::{bounded, Sender};
use image::{ImageBuffer, ImageFormat, Luma};

use super::{ExternalTool, ExternalToolStatus};

const IMAGEJ_FRAME_TITLE: &str = "augur_live";
pub const BUNDLED_IMAGEJ_PLUGIN_JAR: &[u8] = include_bytes!("../../imagej-plugin/AugurBridge_.jar");
pub const BUNDLED_IMAGEJ_PLUGIN_JAR_NAME: &str = "AugurBridge_.jar";
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
    temp_dir: PathBuf,
}

impl ImageJBridge {
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            status: Arc::new(Mutex::new(ExternalToolStatus::Disconnected)),
            frame_tx: None,
            handle: None,
            temp_dir: std::env::temp_dir().join("augur-imagej"),
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

        fs::create_dir_all(&self.temp_dir)
            .map_err(|err| format!("failed to create ImageJ temp dir: {err}"))?;
        *self.status.lock().unwrap() = ExternalToolStatus::Connecting;

        let (tx, rx) = bounded::<FrameEnvelope>(2);
        let host = self.host.clone();
        let port = self.port;
        let status = Arc::clone(&self.status);
        let temp_dir = self.temp_dir.clone();
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

            if let Err(err) = send_macro(&mut stream, "return getVersion();") {
                *status.lock().unwrap() =
                    ExternalToolStatus::Error(format!("handshake failed: {err}"));
                return;
            }

            while let Ok(mut envelope) = rx.recv() {
                while let Ok(next) = rx.try_recv() {
                    envelope = next;
                }

                let frame_path = temp_dir.join("augur_live.tif");
                if let Err(err) = write_frame_tiff(&frame_path, &envelope.frame) {
                    *status.lock().unwrap() =
                        ExternalToolStatus::Error(format!("TIFF export failed: {err}"));
                    return;
                }

                let macro_code = format!(
                    "if (isOpen(\"{title}\")) {{ selectWindow(\"{title}\"); close(); }} \
                     open(\"{path}\"); rename(\"{title}\"); \
                     if (nImages>0) {{ run(\"Set Scale...\", \"distance=1 known={scale} unit=nm\"); }}",
                    title = IMAGEJ_FRAME_TITLE,
                    path = escape_macro_string(&frame_path),
                    scale = envelope.nm_per_pixel
                );
                if let Err(err) = send_macro(&mut stream, &macro_code) {
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

fn send_macro(stream: &mut TcpStream, macro_code: &str) -> Result<(), String> {
    let command = format!("eval {macro_code}\n");
    stream
        .write_all(command.as_bytes())
        .map_err(|err| format!("socket write failed: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("socket flush failed: {err}"))?;
    Ok(())
}

fn write_frame_tiff(path: &Path, frame: &PreviewFrame) -> Result<(), String> {
    let image = ImageBuffer::<Luma<u16>, Vec<u16>>::from_raw(
        u32::from(frame.width),
        u32::from(frame.height),
        frame.pixels.clone(),
    )
    .ok_or_else(|| "frame dimensions do not match pixel buffer".to_string())?;
    image
        .save_with_format(path, ImageFormat::Tiff)
        .map_err(|err| format!("save failed: {err}"))
}

fn escape_macro_string(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

impl Drop for ImageJBridge {
    fn drop(&mut self) {
        self.disconnect();
    }
}
