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

use augur_core::pipeline::CdEvent;
use eframe::egui;
use serde_json::{json, Value};

pub const DEFAULT_PYTHON_INGRESS_PORT: u16 = 57_295;
pub const PYTHON_INGRESS_PROTOCOL_VERSION: u64 = 1;
pub const PACKED_XYPT_RECORD_BYTES: usize = 14;
pub const MAX_CHUNK_EVENTS: usize = 1_048_576;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const STREAM_READ_TIMEOUT: Duration = Duration::from_millis(100);
const GUI_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

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
    pub(crate) reply_tx: mpsc::Sender<std::result::Result<(), String>>,
}

pub struct PythonIngressDatasetRequest {
    pub info: PythonIngressDatasetInfo,
    pub events: Vec<CdEvent>,
    pub(crate) reply_tx: mpsc::Sender<std::result::Result<(), String>>,
}

pub struct PythonIngressServer {
    port: u16,
    status: Arc<Mutex<PythonIngressStatus>>,
    stop: Arc<AtomicBool>,
    start_rx: mpsc::Receiver<PythonIngressStartRequest>,
    dataset_rx: mpsc::Receiver<PythonIngressDatasetRequest>,
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
        let (start_tx, start_rx) = mpsc::channel();
        let (dataset_tx, dataset_rx) = mpsc::channel();
        let worker_status = Arc::clone(&status);
        let worker_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            run_listener(
                listener,
                start_tx,
                dataset_tx,
                worker_status,
                worker_stop,
                ctx,
            );
        });

        Ok(Self {
            port,
            status,
            stop,
            start_rx,
            dataset_rx,
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

    pub fn try_recv_start_request(&self) -> Option<PythonIngressStartRequest> {
        self.start_rx.try_recv().ok()
    }

    pub fn try_recv_dataset_request(&self) -> Option<PythonIngressDatasetRequest> {
        self.dataset_rx.try_recv().ok()
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

fn run_listener(
    listener: TcpListener,
    start_tx: mpsc::Sender<PythonIngressStartRequest>,
    dataset_tx: mpsc::Sender<PythonIngressDatasetRequest>,
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
                if let Err(err) =
                    handle_connection(stream, &start_tx, &dataset_tx, &status, &stop, &ctx)
                {
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
    start_tx: &mpsc::Sender<PythonIngressStartRequest>,
    dataset_tx: &mpsc::Sender<PythonIngressDatasetRequest>,
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
        let (reply_tx, reply_rx) = mpsc::channel();
        start_tx
            .send(PythonIngressStartRequest {
                info: info.clone(),
                reply_tx,
            })
            .map_err(|_| "GUI is no longer accepting Python ingress streams".to_owned())?;
        ctx.request_repaint();

        match wait_for_gui_reply(&reply_rx, stop) {
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

        let events = receive_batches(&info, &mut reader, &mut stream, stop)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        dataset_tx
            .send(PythonIngressDatasetRequest {
                info,
                events,
                reply_tx,
            })
            .map_err(|_| "GUI is no longer accepting Python ingress datasets".to_owned())?;
        ctx.request_repaint();

        match wait_for_gui_reply(&reply_rx, stop) {
            Ok(Ok(())) => write_json(&mut stream, &json!({"type": "finish_ok"}))?,
            Ok(Err(message)) => {
                write_json(
                    &mut stream,
                    &json!({"type": "error", "code": "dataset_rejected", "message": message}),
                )?;
                continue;
            }
            Err(_) => {
                write_json(
                    &mut stream,
                    &json!({
                        "type": "error",
                        "code": "dataset_timeout",
                        "message": "Augur did not open the Python dataset in time",
                    }),
                )?;
                continue;
            }
        }
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
    info: &PythonIngressDatasetInfo,
    reader: &mut BufReader<TcpStream>,
    writer: &mut TcpStream,
    stop: &Arc<AtomicBool>,
) -> std::result::Result<Vec<CdEvent>, String> {
    let capacity = usize::try_from(info.event_count)
        .unwrap_or(usize::MAX)
        .min(MAX_CHUNK_EVENTS);
    let mut events = Vec::with_capacity(capacity);
    while !stop.load(Ordering::Relaxed) {
        let Some(message) = read_json_line(reader, stop)? else {
            return Err("client closed before finish_events".to_owned());
        };
        match message.get("type").and_then(Value::as_str) {
            Some("finish_events") => {
                if u64::try_from(events.len()).unwrap_or(u64::MAX) != info.event_count {
                    return send_protocol_error(
                        writer,
                        "event_count_mismatch",
                        format!(
                            "received {} events, expected {}",
                            events.len(),
                            info.event_count
                        ),
                    )
                    .and_then(|()| Err("Python ingress event count mismatch".to_owned()));
                }
                return Ok(events);
            }
            Some("event_batch") => {
                let batch_events = required_u64(&message, "events")?;
                if batch_events as usize > MAX_CHUNK_EVENTS {
                    send_protocol_error(
                        writer,
                        "chunk_too_large",
                        format!("batch has {batch_events} events; max is {MAX_CHUNK_EVENTS}"),
                    )?;
                    return Err("Python ingress batch exceeds maximum chunk size".to_owned());
                }
                let bytes = required_u64(&message, "bytes")?;
                let expected_bytes = batch_events
                    .checked_mul(PACKED_XYPT_RECORD_BYTES as u64)
                    .ok_or_else(|| "event batch byte count overflow".to_owned())?;
                if bytes != expected_bytes {
                    send_protocol_error(
                        writer,
                        "invalid_batch_size",
                        format!("batch declares {bytes} bytes, expected {expected_bytes}"),
                    )?;
                    return Err("Python ingress batch byte size mismatch".to_owned());
                }
                let bytes = usize::try_from(bytes)
                    .map_err(|_| "batch byte count does not fit this platform".to_owned())?;
                let mut payload = vec![0_u8; bytes];
                read_exact_timeout(reader, &mut payload, stop)?;
                if let Err(err) = append_packed_events(info, &payload, &mut events) {
                    send_protocol_error(writer, "invalid_event_data", err.clone())?;
                    return Err(err);
                }
                write_json(writer, &json!({"type": "batch_ok", "events": batch_events}))?;
            }
            Some(other) => {
                send_protocol_error(
                    writer,
                    "unexpected_message",
                    format!("expected event_batch or finish_events, got {other}"),
                )?;
                return Err(format!("unexpected Python ingress message {other}"));
            }
            None => {
                send_protocol_error(writer, "missing_type", "message is missing a type field")?;
                return Err("Python ingress message is missing a type field".to_owned());
            }
        }
    }

    Ok(events)
}

fn wait_for_gui_reply(
    reply_rx: &mpsc::Receiver<std::result::Result<(), String>>,
    stop: &AtomicBool,
) -> std::result::Result<std::result::Result<(), String>, mpsc::RecvTimeoutError> {
    let mut remaining = GUI_REPLY_TIMEOUT;
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

fn append_packed_events(
    info: &PythonIngressDatasetInfo,
    payload: &[u8],
    out: &mut Vec<CdEvent>,
) -> std::result::Result<(), String> {
    let remaining_events = info
        .event_count
        .saturating_sub(u64::try_from(out.len()).unwrap_or(u64::MAX));
    let payload_events = payload.len() / PACKED_XYPT_RECORD_BYTES;
    if u64::try_from(payload_events).unwrap_or(u64::MAX) > remaining_events {
        return Err(format!(
            "received more events than start_events declared: {} + {payload_events} > {}",
            out.len(),
            info.event_count
        ));
    }

    out.reserve(payload_events);
    for chunk in payload.chunks_exact(PACKED_XYPT_RECORD_BYTES) {
        let event = decode_packed_xypt_event(chunk);
        if event.x >= info.width || event.y >= info.height {
            return Err(format!(
                "event coordinate ({}, {}) exceeds published geometry {}x{}",
                event.x, event.y, info.width, info.height
            ));
        }
        out.push(event);
    }
    Ok(())
}

fn decode_packed_xypt_event(bytes: &[u8]) -> CdEvent {
    CdEvent {
        x: u16::from_le_bytes([bytes[0], bytes[1]]),
        y: u16::from_le_bytes([bytes[2], bytes[3]]),
        polarity: bytes[4] != 0,
        timestamp: u64::from_le_bytes([
            bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
        ]),
    }
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

    fn packed_event(
        x: u16,
        y: u16,
        polarity: u8,
        timestamp: u64,
    ) -> [u8; PACKED_XYPT_RECORD_BYTES] {
        let mut out = [0_u8; PACKED_XYPT_RECORD_BYTES];
        out[..2].copy_from_slice(&x.to_le_bytes());
        out[2..4].copy_from_slice(&y.to_le_bytes());
        out[4] = polarity;
        out[6..14].copy_from_slice(&timestamp.to_le_bytes());
        out
    }

    #[test]
    fn append_packed_events_decodes_xypt_records() {
        let info = PythonIngressDatasetInfo {
            name: Some("unit".into()),
            width: 8,
            height: 6,
            event_count: 2,
            timestamp_start_us: 10,
            timestamp_end_us: 20,
        };
        let mut payload = Vec::new();
        payload.extend_from_slice(&packed_event(1, 2, 0, 10));
        payload.extend_from_slice(&packed_event(7, 5, 1, 20));
        let mut events = Vec::new();

        append_packed_events(&info, &payload, &mut events).expect("payload decodes");

        assert_eq!(
            events,
            vec![
                CdEvent {
                    x: 1,
                    y: 2,
                    timestamp: 10,
                    polarity: false,
                },
                CdEvent {
                    x: 7,
                    y: 5,
                    timestamp: 20,
                    polarity: true,
                },
            ]
        );
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
                if let Some(request) = server.try_recv_start_request() {
                    break request;
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not publish start request"
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        let PythonIngressStartRequest { info, reply_tx } = request;
        assert_eq!(info.name.as_deref(), Some("unit-test"));
        assert_eq!((info.width, info.height), (8, 6));
        reply_tx.send(Ok(())).expect("ack start");
        assert_eq!(read_json_test(&mut reader)["type"], "start_ok");

        let mut payload = Vec::new();
        payload.extend_from_slice(&packed_event(1, 2, 0, 10));
        payload.extend_from_slice(&packed_event(7, 5, 1, 20));
        write_json_test(
            &mut stream,
            json!({"type": "event_batch", "events": 2, "bytes": payload.len()}),
        );
        stream.write_all(&payload).expect("payload writes");
        let batch_ok = read_json_test(&mut reader);
        assert_eq!(batch_ok["type"], "batch_ok");
        assert_eq!(batch_ok["events"], 2);

        write_json_test(&mut stream, json!({"type": "finish_events"}));
        let dataset = {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(request) = server.try_recv_dataset_request() {
                    break request;
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not publish dataset request"
                );
                thread::sleep(Duration::from_millis(10));
            }
        };
        let PythonIngressDatasetRequest {
            info,
            events,
            reply_tx,
        } = dataset;
        assert_eq!(info.name.as_deref(), Some("unit-test"));
        assert_eq!(
            events,
            vec![
                CdEvent {
                    x: 1,
                    y: 2,
                    timestamp: 10,
                    polarity: false,
                },
                CdEvent {
                    x: 7,
                    y: 5,
                    timestamp: 20,
                    polarity: true,
                },
            ]
        );
        reply_tx.send(Ok(())).expect("ack dataset");
        assert_eq!(read_json_test(&mut reader)["type"], "finish_ok");
        server.stop();
    }
}
