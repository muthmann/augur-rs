use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use augur_core::{
    camera::{DeviceInfo, EventCamera, PacketStreamCamera},
    config::CameraConfig,
    CameraError, Result,
};
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender};
use eframe::egui;
use serde_json::{json, Value};

pub const DEFAULT_PYTHON_INGRESS_PORT: u16 = 57_295;
pub const PYTHON_INGRESS_PROTOCOL_VERSION: u64 = 1;
pub const PACKED_XYPT_RECORD_BYTES: usize = 14;
pub const MAX_CHUNK_EVENTS: usize = 1_048_576;

const PACKET_QUEUE_CAPACITY: usize = 8;
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STREAM_READ_TIMEOUT: Duration = Duration::from_millis(100);
const START_ACK_TIMEOUT: Duration = Duration::from_secs(30);
const CAMERA_RECV_TIMEOUT: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonIngressStatus {
    Listening { port: u16 },
    Connected { peer: String },
    Error(String),
}

impl PythonIngressStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Listening { port } => format!("Listening on 127.0.0.1:{port}"),
            Self::Connected { peer } => format!("Connected: {peer}"),
            Self::Error(err) => format!("Error: {err}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PythonIngressDatasetInfo {
    pub name: Option<String>,
    pub width: u16,
    pub height: u16,
    pub event_count: u64,
    pub timestamp_start_us: u64,
    pub timestamp_end_us: u64,
}

pub struct PythonIngressStartRequest {
    pub info: PythonIngressDatasetInfo,
    pub camera: PythonIngressCamera,
    pub(crate) reply_tx: mpsc::Sender<std::result::Result<(), String>>,
}

pub struct PythonIngressServer {
    port: u16,
    status: Arc<Mutex<PythonIngressStatus>>,
    stop: Arc<AtomicBool>,
    request_rx: mpsc::Receiver<PythonIngressStartRequest>,
    handle: Option<JoinHandle<()>>,
}

impl PythonIngressServer {
    pub fn start(ctx: egui::Context, port: u16) -> std::result::Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .map_err(|err| format!("Python ingress bind failed: {err}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("Python ingress listener setup failed: {err}"))?;
        let port = listener
            .local_addr()
            .map_err(|err| format!("Python ingress listener address failed: {err}"))?
            .port();

        let status = Arc::new(Mutex::new(PythonIngressStatus::Listening { port }));
        let stop = Arc::new(AtomicBool::new(false));
        let (request_tx, request_rx) = mpsc::channel();
        let worker_status = Arc::clone(&status);
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            run_listener(listener, request_tx, worker_status, worker_stop, ctx);
        });

        Ok(Self {
            port,
            status,
            stop,
            request_rx,
            handle: Some(handle),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> PythonIngressStatus {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| PythonIngressStatus::Error("status lock poisoned".into()))
    }

    pub fn try_recv_request(&self) -> Option<PythonIngressStartRequest> {
        self.request_rx.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PythonIngressServer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct PythonIngressCamera {
    rx: Receiver<Vec<u8>>,
    current: Vec<u8>,
    offset: usize,
    stopped: Arc<AtomicBool>,
    info: PythonIngressDatasetInfo,
}

impl PythonIngressCamera {
    fn new(
        rx: Receiver<Vec<u8>>,
        stopped: Arc<AtomicBool>,
        info: PythonIngressDatasetInfo,
    ) -> Self {
        Self {
            rx,
            current: Vec::new(),
            offset: 0,
            stopped,
            info,
        }
    }
}

impl EventCamera for PythonIngressCamera {
    fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
        Ok(())
    }

    fn start_streaming(&mut self) -> Result<()> {
        Ok(())
    }

    fn stop_streaming(&mut self) -> Result<()> {
        self.stopped.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor: "Python".into(),
            model: self
                .info
                .name
                .clone()
                .unwrap_or_else(|| "NumPy event ingress".into()),
            serial: None,
            firmware: None,
            compatible: Some("packed_xypt_v1".into()),
        }
    }
}

impl PacketStreamCamera for PythonIngressCamera {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        while self.offset >= self.current.len() {
            if self.stopped.load(Ordering::Relaxed) {
                return Err(CameraError::Eof);
            }
            match self.rx.recv_timeout(CAMERA_RECV_TIMEOUT) {
                Ok(packet) => {
                    self.current = packet;
                    self.offset = 0;
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(CameraError::Timeout(
                        "waiting for Python ingress event batch".into(),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => return Err(CameraError::Eof),
            }
        }

        let remaining = self.current.len() - self.offset;
        let copied = remaining.min(buf.len());
        buf[..copied].copy_from_slice(&self.current[self.offset..self.offset + copied]);
        self.offset += copied;
        Ok(copied)
    }
}

fn run_listener(
    listener: TcpListener,
    request_tx: mpsc::Sender<PythonIngressStartRequest>,
    status: Arc<Mutex<PythonIngressStatus>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                set_status(
                    &status,
                    PythonIngressStatus::Connected {
                        peer: peer.to_string(),
                    },
                );
                ctx.request_repaint();
                if let Err(err) = handle_connection(stream, &request_tx, &status, &stop, &ctx) {
                    set_status(&status, PythonIngressStatus::Error(err));
                    ctx.request_repaint();
                }
                if !stop.load(Ordering::Relaxed) {
                    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
                    set_status(&status, PythonIngressStatus::Listening { port });
                    ctx.request_repaint();
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(err) => {
                set_status(
                    &status,
                    PythonIngressStatus::Error(format!("accept failed: {err}")),
                );
                ctx.request_repaint();
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    request_tx: &mpsc::Sender<PythonIngressStartRequest>,
    status: &Arc<Mutex<PythonIngressStatus>>,
    stop: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> std::result::Result<(), String> {
    stream
        .set_read_timeout(Some(STREAM_READ_TIMEOUT))
        .map_err(|err| format!("set read timeout failed: {err}"))?;
    let reader_stream = stream
        .try_clone()
        .map_err(|err| format!("clone stream failed: {err}"))?;
    let mut reader = BufReader::new(reader_stream);

    let hello = read_json_line(&mut reader, stop)?
        .ok_or_else(|| "client closed before hello".to_owned())?;
    require_type(&hello, "hello")?;
    let protocol = required_u64(&hello, "protocol")?;
    if protocol != PYTHON_INGRESS_PROTOCOL_VERSION {
        write_json(
            &mut stream,
            &json!({
                "type": "error",
                "code": "unsupported_protocol",
                "message": format!(
                    "Augur supports protocol {}, got {protocol}",
                    PYTHON_INGRESS_PROTOCOL_VERSION
                ),
            }),
        )?;
        return Ok(());
    }
    write_json(
        &mut stream,
        &json!({
            "type": "hello_ok",
            "protocol": PYTHON_INGRESS_PROTOCOL_VERSION,
            "server": "augur",
            "max_chunk_events": MAX_CHUNK_EVENTS,
        }),
    )?;

    while !stop.load(Ordering::Relaxed) {
        let Some(start) = read_json_line(&mut reader, stop)? else {
            return Ok(());
        };
        require_type(&start, "start_events")?;
        let info = parse_dataset_info(&start)?;
        let (packet_tx, packet_rx) = bounded(PACKET_QUEUE_CAPACITY);
        let stream_stopped = Arc::new(AtomicBool::new(false));
        let camera = PythonIngressCamera::new(packet_rx, Arc::clone(&stream_stopped), info.clone());
        let (reply_tx, reply_rx) = mpsc::channel();
        request_tx
            .send(PythonIngressStartRequest {
                info,
                camera,
                reply_tx,
            })
            .map_err(|_| "GUI is no longer accepting Python ingress streams".to_owned())?;
        ctx.request_repaint();

        match wait_for_start_reply(&reply_rx, stop) {
            Ok(Ok(())) => write_json(&mut stream, &json!({"type": "start_ok"}))?,
            Ok(Err(message)) => {
                write_json(
                    &mut stream,
                    &json!({"type": "error", "code": "start_rejected", "message": message}),
                )?;
                continue;
            }
            Err(_) => {
                write_json(
                    &mut stream,
                    &json!({
                        "type": "error",
                        "code": "start_timeout",
                        "message": "Augur did not accept the Python stream in time",
                    }),
                )?;
                continue;
            }
        }

        receive_batches(&mut reader, &mut stream, packet_tx, &stream_stopped, stop)?;
        set_status(
            status,
            PythonIngressStatus::Connected {
                peer: stream
                    .peer_addr()
                    .map(|addr| addr.to_string())
                    .unwrap_or_else(|_| "unknown".into()),
            },
        );
        ctx.request_repaint();
    }

    Ok(())
}

fn receive_batches(
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    packet_tx: Sender<Vec<u8>>,
    stream_stopped: &Arc<AtomicBool>,
    stop: &Arc<AtomicBool>,
) -> std::result::Result<(), String> {
    while !stop.load(Ordering::Relaxed) {
        let Some(message) = read_json_line(reader, stop)? else {
            return Ok(());
        };
        match message.get("type").and_then(Value::as_str) {
            Some("finish_events") => {
                drop(packet_tx);
                write_json(writer, &json!({"type": "finish_ok"}))?;
                return Ok(());
            }
            Some("event_batch") => {
                let events = required_u64(&message, "events")?;
                if events as usize > MAX_CHUNK_EVENTS {
                    return send_protocol_error(
                        writer,
                        "chunk_too_large",
                        format!("batch has {events} events; max is {MAX_CHUNK_EVENTS}"),
                    );
                }
                let bytes = required_u64(&message, "bytes")?;
                let expected_bytes = events
                    .checked_mul(PACKED_XYPT_RECORD_BYTES as u64)
                    .ok_or_else(|| "event batch byte count overflow".to_owned())?;
                if bytes != expected_bytes {
                    return send_protocol_error(
                        writer,
                        "invalid_batch_size",
                        format!("batch declares {bytes} bytes, expected {expected_bytes}"),
                    );
                }
                let bytes = usize::try_from(bytes)
                    .map_err(|_| "batch byte count does not fit this platform".to_owned())?;
                let mut payload = vec![0_u8; bytes];
                read_exact_timeout(reader, &mut payload, stop)?;
                send_packet(&packet_tx, payload, stream_stopped, stop)?;
                write_json(writer, &json!({"type": "batch_ok", "events": events}))?;
            }
            Some(other) => {
                return send_protocol_error(
                    writer,
                    "unexpected_message",
                    format!("expected event_batch or finish_events, got {other}"),
                );
            }
            None => {
                return send_protocol_error(
                    writer,
                    "missing_type",
                    "message is missing a type field",
                );
            }
        }
    }

    Ok(())
}

fn wait_for_start_reply(
    reply_rx: &mpsc::Receiver<std::result::Result<(), String>>,
    stop: &AtomicBool,
) -> std::result::Result<std::result::Result<(), String>, mpsc::RecvTimeoutError> {
    let mut remaining = START_ACK_TIMEOUT;
    while !stop.load(Ordering::Relaxed) {
        let wait = remaining.min(STREAM_READ_TIMEOUT);
        match reply_rx.recv_timeout(wait) {
            Ok(reply) => return Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) if remaining > wait => {
                remaining -= wait;
            }
            Err(err) => return Err(err),
        }
    }
    Err(mpsc::RecvTimeoutError::Disconnected)
}

fn send_packet(
    packet_tx: &Sender<Vec<u8>>,
    mut payload: Vec<u8>,
    stream_stopped: &AtomicBool,
    stop: &AtomicBool,
) -> std::result::Result<(), String> {
    loop {
        if stop.load(Ordering::Relaxed) || stream_stopped.load(Ordering::Relaxed) {
            return Err("pipeline stopped before receiving the batch".to_owned());
        }

        match packet_tx.send_timeout(payload, STREAM_READ_TIMEOUT) {
            Ok(()) => return Ok(()),
            Err(SendTimeoutError::Timeout(returned_payload)) => {
                payload = returned_payload;
            }
            Err(SendTimeoutError::Disconnected(_)) => {
                return Err("pipeline stopped before receiving the batch".to_owned());
            }
        }
    }
}

fn parse_dataset_info(message: &Value) -> std::result::Result<PythonIngressDatasetInfo, String> {
    let record_format = required_str(message, "record_format")?;
    if record_format != "packed_xypt_v1" {
        return Err(format!("unsupported record_format {record_format:?}"));
    }
    let record_bytes = required_u64(message, "record_bytes")?;
    if record_bytes != PACKED_XYPT_RECORD_BYTES as u64 {
        return Err(format!(
            "record_bytes must be {}, got {record_bytes}",
            PACKED_XYPT_RECORD_BYTES
        ));
    }
    let time_unit = required_str(message, "time_unit")?;
    if time_unit != "us" {
        return Err(format!("time_unit must be \"us\", got {time_unit:?}"));
    }
    let geometry = message
        .get("geometry")
        .and_then(Value::as_array)
        .ok_or_else(|| "start_events.geometry must be [width, height]".to_owned())?;
    if geometry.len() != 2 {
        return Err("start_events.geometry must contain width and height".into());
    }
    let width = u16::try_from(
        geometry[0]
            .as_u64()
            .ok_or_else(|| "geometry width must be an unsigned integer".to_owned())?,
    )
    .map_err(|_| "geometry width must fit u16".to_owned())?;
    let height = u16::try_from(
        geometry[1]
            .as_u64()
            .ok_or_else(|| "geometry height must be an unsigned integer".to_owned())?,
    )
    .map_err(|_| "geometry height must fit u16".to_owned())?;
    if width == 0 || height == 0 {
        return Err("geometry width and height must be positive".into());
    }

    Ok(PythonIngressDatasetInfo {
        name: message
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        width,
        height,
        event_count: required_u64(message, "event_count")?,
        timestamp_start_us: required_u64(message, "timestamp_start_us")?,
        timestamp_end_us: required_u64(message, "timestamp_end_us")?,
    })
}

fn read_json_line(
    reader: &mut BufReader<TcpStream>,
    stop: &AtomicBool,
) -> std::result::Result<Option<Value>, String> {
    let mut line = String::new();
    loop {
        match reader.read_line(&mut line) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                return serde_json::from_str(&line)
                    .map(Some)
                    .map_err(|err| format!("invalid JSON message: {err}"));
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Relaxed) {
                    return Ok(None);
                }
            }
            Err(err) => return Err(format!("socket read failed: {err}")),
        }
    }
}

fn read_exact_timeout(
    reader: &mut BufReader<TcpStream>,
    mut out: &mut [u8],
    stop: &AtomicBool,
) -> std::result::Result<(), String> {
    while !out.is_empty() {
        match reader.read(out) {
            Ok(0) => return Err("client closed during binary payload".into()),
            Ok(n) => {
                let tmp = out;
                out = &mut tmp[n..];
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Relaxed) {
                    return Err("Python ingress stopped while reading payload".into());
                }
            }
            Err(err) => return Err(format!("payload read failed: {err}")),
        }
    }
    Ok(())
}

fn write_json(writer: &mut TcpStream, message: &Value) -> std::result::Result<(), String> {
    let mut payload =
        serde_json::to_vec(message).map_err(|err| format!("JSON encode failed: {err}"))?;
    payload.push(b'\n');
    writer
        .write_all(&payload)
        .map_err(|err| format!("socket write failed: {err}"))?;
    writer
        .flush()
        .map_err(|err| format!("socket flush failed: {err}"))
}

fn send_protocol_error(
    writer: &mut TcpStream,
    code: &str,
    message: impl Into<String>,
) -> std::result::Result<(), String> {
    write_json(
        writer,
        &json!({"type": "error", "code": code, "message": message.into()}),
    )
}

fn require_type(message: &Value, expected: &str) -> std::result::Result<(), String> {
    let actual = message.get("type").and_then(Value::as_str);
    if actual == Some(expected) {
        Ok(())
    } else {
        Err(format!(
            "expected message type {expected:?}, got {actual:?}"
        ))
    }
}

fn required_str<'a>(message: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    message
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{key} must be a string"))
}

fn required_u64(message: &Value, key: &str) -> std::result::Result<u64, String> {
    message
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("{key} must be an unsigned integer"))
}

fn set_status(status: &Arc<Mutex<PythonIngressStatus>>, next: PythonIngressStatus) {
    if let Ok(mut status) = status.lock() {
        *status = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn write_json_test(stream: &mut TcpStream, value: Value) {
        let mut bytes = serde_json::to_vec(&value).expect("test JSON encodes");
        bytes.push(b'\n');
        stream.write_all(&bytes).expect("test JSON writes");
    }

    fn read_json_test(reader: &mut BufReader<TcpStream>) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).expect("test JSON reads");
        serde_json::from_str(&line).expect("test JSON decodes")
    }

    #[test]
    fn camera_reads_batches_in_buffer_sized_chunks() {
        let (tx, rx) = bounded(1);
        let info = PythonIngressDatasetInfo {
            name: Some("unit".into()),
            width: 2,
            height: 2,
            event_count: 2,
            timestamp_start_us: 1,
            timestamp_end_us: 2,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let mut camera = PythonIngressCamera::new(rx, stop, info);
        tx.send((0_u8..28).collect()).expect("send test payload");
        drop(tx);

        let mut first = [0_u8; 10];
        let mut second = [0_u8; 32];

        assert_eq!(camera.read_packet(&mut first).expect("first read"), 10);
        assert_eq!(&first, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(camera.read_packet(&mut second).expect("second read"), 18);
        assert_eq!(&second[..18], &(10_u8..28).collect::<Vec<_>>());
        assert!(matches!(
            camera.read_packet(&mut second),
            Err(CameraError::Eof)
        ));
    }

    #[test]
    fn parse_dataset_info_accepts_evt3_start_message() {
        let info = parse_dataset_info(&json!({
            "type": "start_events",
            "name": "unit-test",
            "geometry": [1280, 720],
            "event_count": 5,
            "time_unit": "us",
            "record_format": "packed_xypt_v1",
            "record_bytes": 14,
            "timestamp_start_us": 100,
            "timestamp_end_us": 500,
        }))
        .expect("valid evt3 connector message");

        assert_eq!(info.name.as_deref(), Some("unit-test"));
        assert_eq!((info.width, info.height), (1280, 720));
        assert_eq!(info.event_count, 5);
        assert_eq!(info.timestamp_start_us, 100);
        assert_eq!(info.timestamp_end_us, 500);
    }

    #[test]
    fn parse_dataset_info_rejects_wrong_record_format() {
        let err = parse_dataset_info(&json!({
            "type": "start_events",
            "geometry": [1280, 720],
            "event_count": 5,
            "time_unit": "us",
            "record_format": "columnar",
            "record_bytes": 14,
            "timestamp_start_us": 100,
            "timestamp_end_us": 500,
        }))
        .expect_err("wrong record format should fail");

        assert!(err.contains("unsupported record_format"));
    }

    #[test]
    fn server_speaks_evt3_protocol_and_queues_batch() {
        let ctx = egui::Context::default();
        let mut server = PythonIngressServer::start(ctx, 0).expect("server starts");
        let mut stream = TcpStream::connect(("127.0.0.1", server.port())).expect("client connects");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set test read timeout");
        let mut reader = BufReader::new(stream.try_clone().expect("clone test stream"));

        write_json_test(
            &mut stream,
            json!({
                "type": "hello",
                "protocol": 1,
                "client": "evt3-python",
                "client_version": "test",
            }),
        );
        assert_eq!(read_json_test(&mut reader)["type"], "hello_ok");

        write_json_test(
            &mut stream,
            json!({
                "type": "start_events",
                "name": "unit-test",
                "geometry": [8, 6],
                "event_count": 2,
                "time_unit": "us",
                "record_format": "packed_xypt_v1",
                "record_bytes": 14,
                "timestamp_start_us": 10,
                "timestamp_end_us": 20,
            }),
        );

        let request = {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(request) = server.try_recv_request() {
                    break request;
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not publish start request"
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        let PythonIngressStartRequest {
            info,
            mut camera,
            reply_tx,
        } = request;
        assert_eq!(info.name.as_deref(), Some("unit-test"));
        assert_eq!((info.width, info.height), (8, 6));
        reply_tx.send(Ok(())).expect("ack start");
        assert_eq!(read_json_test(&mut reader)["type"], "start_ok");

        let payload: Vec<u8> = (0_u8..28).collect();
        write_json_test(
            &mut stream,
            json!({"type": "event_batch", "events": 2, "bytes": payload.len()}),
        );
        stream.write_all(&payload).expect("payload writes");
        let batch_ok = read_json_test(&mut reader);
        assert_eq!(batch_ok["type"], "batch_ok");
        assert_eq!(batch_ok["events"], 2);

        let mut buf = [0_u8; 28];
        assert_eq!(
            camera.read_packet(&mut buf).expect("camera receives batch"),
            28
        );
        assert_eq!(&buf[..], payload.as_slice());

        write_json_test(&mut stream, json!({"type": "finish_events"}));
        assert_eq!(read_json_test(&mut reader)["type"], "finish_ok");
        server.stop();
    }
}
