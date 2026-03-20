use std::{
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{
    bounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender, TryRecvError, TrySendError,
};
use evt3_core::{CdEvent as Evt3CdEvent, Evt3Decoder, TriggerEvent as Evt3TriggerEvent};

use crate::{camera::PacketStreamCamera, config::CameraConfig, CameraError, Result};

pub const BUF_SIZE: usize = 65_536;
pub const N_BUFFERS: usize = 8;
const CURRENT_RATE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CdEvent {
    pub x: u16,
    pub y: u16,
    pub timestamp: u64,
    pub polarity: bool,
}

pub trait PreviewDecoder: Send + 'static {
    fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()>;

    fn finish_stream(&mut self) -> Result<()> {
        Ok(())
    }

    fn estimate_event_count(bytes: &[u8]) -> u64
    where
        Self: Sized,
    {
        let _ = bytes;
        0
    }
}

#[derive(Debug, Default)]
pub struct Evt3CorePreviewDecoder {
    inner: Evt3Decoder,
    cd_scratch: Vec<Evt3CdEvent>,
    trigger_scratch: Vec<Evt3TriggerEvent>,
}

impl PreviewDecoder for Evt3CorePreviewDecoder {
    fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()> {
        out.clear();
        self.cd_scratch.clear();
        self.trigger_scratch.clear();

        self.inner
            .decode_bytes(bytes, &mut self.cd_scratch, &mut self.trigger_scratch)
            .map_err(|e| CameraError::Other(format!("evt3 decode failed: {e}")))?;

        out.extend(self.cd_scratch.iter().map(|event| CdEvent {
            x: event.x,
            y: event.y,
            timestamp: event.timestamp,
            polarity: event.polarity != 0,
        }));

        Ok(())
    }

    fn finish_stream(&mut self) -> Result<()> {
        self.inner.finish_stream_lenient();
        Ok(())
    }

    fn estimate_event_count(bytes: &[u8]) -> u64 {
        estimate_evt3_cd_events(bytes)
    }
}

#[derive(Debug, Clone)]
pub struct PreviewFrame {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u16>,
    pub pixels_on: Vec<u16>,
    pub pixels_off: Vec<u16>,
    pub on_count: u64,
    pub off_count: u64,
    pub events: Option<Vec<CdEvent>>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub output_path: Option<PathBuf>,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub write_evt3_header: bool,
}

impl PipelineOptions {
    pub fn new(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: Some(output_path.into()),
            sensor_width: 1280,
            sensor_height: 720,
            write_evt3_header: true,
        }
    }

    pub fn preview_only(width: u16, height: u16) -> Self {
        Self {
            output_path: None,
            sensor_width: width,
            sensor_height: height,
            write_evt3_header: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineStatsSnapshot {
    pub elapsed_s: f64,
    pub bytes_total: u64,
    pub events_total: u64,
    pub mb_per_s: f64,
    pub mev_per_s: f64,
}

#[derive(Debug)]
struct PipelineStatsInner {
    started: Instant,
    bytes_total: u64,
    events_total: u64,
    recent_samples: VecDeque<PipelineStatsSample>,
}

#[derive(Debug, Clone, Copy)]
struct PipelineStatsSample {
    at: Instant,
    bytes: u64,
    events: u64,
}

impl PipelineStatsInner {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            bytes_total: 0,
            events_total: 0,
            recent_samples: VecDeque::new(),
        }
    }

    fn record_packet(&mut self, now: Instant, bytes: u64, events: u64) {
        self.bytes_total += bytes;
        self.events_total += events;
        self.recent_samples.push_back(PipelineStatsSample {
            at: now,
            bytes,
            events,
        });
        self.prune_recent(now);
    }

    fn snapshot(&mut self) -> PipelineStatsSnapshot {
        self.snapshot_at(Instant::now())
    }

    fn snapshot_at(&mut self, now: Instant) -> PipelineStatsSnapshot {
        self.prune_recent(now);

        let elapsed_s = now.duration_since(self.started).as_secs_f64().max(1e-6);
        let current_window_s = now
            .duration_since(self.started)
            .min(CURRENT_RATE_WINDOW)
            .as_secs_f64()
            .max(1e-6);
        let recent_bytes: u64 = self.recent_samples.iter().map(|sample| sample.bytes).sum();
        let recent_events: u64 = self.recent_samples.iter().map(|sample| sample.events).sum();
        PipelineStatsSnapshot {
            elapsed_s,
            bytes_total: self.bytes_total,
            events_total: self.events_total,
            mb_per_s: recent_bytes as f64 / current_window_s / (1024.0 * 1024.0),
            mev_per_s: recent_events as f64 / current_window_s / 1_000_000.0,
        }
    }

    fn prune_recent(&mut self, now: Instant) {
        let cutoff = now.checked_sub(CURRENT_RATE_WINDOW).unwrap_or(self.started);
        while matches!(self.recent_samples.front(), Some(sample) if sample.at < cutoff) {
            self.recent_samples.pop_front();
        }
    }
}

type UsbBuffer = Box<[u8; BUF_SIZE]>;

struct DiskChunk {
    buf: UsbBuffer,
    len: usize,
}

struct PreviewChunk {
    buf: UsbBuffer,
    len: usize,
}

pub struct PipelineController {
    pub frame_rx: Receiver<PreviewFrame>,
    pub settings_tx: Sender<CameraConfig>,
    pub acq_time_us: Arc<AtomicU64>,
    pub raw_events_needed: Arc<AtomicBool>,
    error_rx: Receiver<String>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<PipelineStatsInner>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl PipelineController {
    pub fn stats_snapshot(&self) -> PipelineStatsSnapshot {
        self.stats
            .lock()
            .map(|mut s| s.snapshot())
            .unwrap_or_else(|_| PipelineStatsSnapshot::default())
    }

    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn try_recv_error(&self) -> Option<String> {
        self.error_rx.try_recv().ok()
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    pub fn shutdown(mut self) -> Result<()> {
        self.request_stop();
        let mut had_panic = false;
        while let Some(handle) = self.threads.pop() {
            if handle.join().is_err() {
                had_panic = true;
            }
        }
        if had_panic {
            return Err(CameraError::Other(
                "one or more pipeline threads panicked".into(),
            ));
        }
        Ok(())
    }
}

pub fn spawn_pipeline<C, D>(
    camera: C,
    mut decoder: D,
    initial_config: CameraConfig,
    options: PipelineOptions,
) -> Result<PipelineController>
where
    C: PacketStreamCamera + 'static,
    D: PreviewDecoder,
{
    initial_config.validate(options.sensor_width, options.sensor_height)?;

    let recording = options.output_path.is_some();

    if let Some(ref output_path) = options.output_path {
        if let Some(config_path) = recording_config_path(output_path) {
            initial_config.save_to_path(config_path)?;
        }
    }

    let disk_writer = if let Some(ref output_path) = options.output_path {
        Some(prepare_output_writer(
            output_path,
            options.write_evt3_header,
            options.sensor_width,
            options.sensor_height,
        )?)
    } else {
        None
    };

    let (pool_tx, pool_rx) = bounded::<UsbBuffer>(N_BUFFERS);
    for _ in 0..N_BUFFERS {
        pool_tx
            .send(Box::new([0_u8; BUF_SIZE]))
            .map_err(|e| CameraError::Channel(format!("buffer pool init failed: {e}")))?;
    }
    let (preview_pool_tx, preview_pool_rx) = bounded::<UsbBuffer>(N_BUFFERS);
    for _ in 0..N_BUFFERS {
        preview_pool_tx
            .send(Box::new([0_u8; BUF_SIZE]))
            .map_err(|e| CameraError::Channel(format!("preview pool init failed: {e}")))?;
    }

    let (disk_tx, disk_rx) = if recording {
        let (tx, rx) = bounded::<DiskChunk>(N_BUFFERS);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (preview_tx, preview_rx) = bounded::<PreviewChunk>(N_BUFFERS);
    let (frame_tx, frame_rx) = bounded::<PreviewFrame>(2);
    let (settings_tx, settings_rx) = bounded::<CameraConfig>(8);
    let (error_tx, error_rx) = bounded::<String>(32);

    let acq_time_us = Arc::new(AtomicU64::new(50_000));
    let raw_events_needed = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(PipelineStatsInner::new()));

    let stop_usb = Arc::clone(&stop);
    let stop_preview = Arc::clone(&stop);
    let stats_usb = Arc::clone(&stats);
    let acq_preview = Arc::clone(&acq_time_us);
    let raw_events_preview = Arc::clone(&raw_events_needed);
    let error_usb = error_tx.clone();
    let error_preview = error_tx.clone();

    let usb_pool_tx = pool_tx.clone();
    let preview_pool_tx_usb = preview_pool_tx.clone();
    let stats_preview = Arc::clone(&stats);

    let usb_thread = thread::spawn(move || {
        let mut camera = camera;

        if let Err(e) = camera.configure(&initial_config) {
            report_pipeline_error(
                &error_usb,
                &stop_usb,
                "usb",
                format!("initial camera configure failed: {e}"),
            );
            return;
        }

        if let Err(e) = camera.start_streaming() {
            report_pipeline_error(
                &error_usb,
                &stop_usb,
                "usb",
                format!("camera start_streaming failed: {e}"),
            );
            return;
        }

        while !stop_usb.load(Ordering::Relaxed) {
            loop {
                match settings_rx.try_recv() {
                    Ok(cfg) => {
                        if let Err(e) = camera.configure(&cfg) {
                            let _ =
                                error_usb.try_send(format!("usb: runtime reconfigure failed: {e}"));
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }

            let mut buf = match pool_rx.recv_timeout(Duration::from_millis(10)) {
                Ok(b) => b,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };

            let len = match camera.read_packet(&mut buf[..]) {
                Ok(n) => n,
                Err(CameraError::Timeout(_)) => {
                    let _ = usb_pool_tx.send(buf);
                    continue;
                }
                Err(CameraError::Eof) => {
                    let _ = usb_pool_tx.send(buf);
                    stop_usb.store(true, Ordering::Relaxed);
                    break;
                }
                Err(e) => {
                    report_pipeline_error(
                        &error_usb,
                        &stop_usb,
                        "usb",
                        format!("stream read failed: {e}"),
                    );
                    let _ = usb_pool_tx.send(buf);
                    break;
                }
            };

            if len == 0 {
                let _ = usb_pool_tx.send(buf);
                continue;
            }

            if let Ok(mut s) = stats_usb.lock() {
                let now = Instant::now();
                s.record_packet(now, len as u64, 0);
            }

            if let Ok(mut preview_buf) = preview_pool_rx.try_recv() {
                preview_buf[..len].copy_from_slice(&buf[..len]);
                match preview_tx.try_send(PreviewChunk {
                    buf: preview_buf,
                    len,
                }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(chunk)) | Err(TrySendError::Disconnected(chunk)) => {
                        let _ = preview_pool_tx_usb.try_send(chunk.buf);
                    }
                }
            }

            if let Some(ref disk_tx) = disk_tx {
                match disk_tx.send_timeout(DiskChunk { buf, len }, Duration::from_millis(50)) {
                    Ok(_) => {}
                    Err(SendTimeoutError::Timeout(chunk)) => match disk_tx.send(chunk) {
                        Ok(_) => {}
                        Err(e) => {
                            report_pipeline_error(
                                &error_usb,
                                &stop_usb,
                                "usb",
                                format!("disk channel send failed: {e}"),
                            );
                            break;
                        }
                    },
                    Err(SendTimeoutError::Disconnected(chunk)) => {
                        let _ = usb_pool_tx.send(chunk.buf);
                        report_pipeline_error(
                            &error_usb,
                            &stop_usb,
                            "usb",
                            "disk channel disconnected".to_string(),
                        );
                        break;
                    }
                }
            } else {
                let _ = usb_pool_tx.send(buf);
            }
        }

        if let Err(e) = camera.stop_streaming() {
            report_pipeline_error(
                &error_usb,
                &stop_usb,
                "usb",
                format!("camera stop_streaming failed: {e}"),
            );
        }
    });

    let mut threads = vec![usb_thread];

    if let (Some(disk_rx), Some(mut writer)) = (disk_rx, disk_writer) {
        let stop_disk = Arc::clone(&stop);
        let error_disk = error_tx.clone();
        let disk_pool_tx = pool_tx.clone();

        let disk_thread = thread::spawn(move || {
            loop {
                match disk_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(chunk) => {
                        if let Err(e) = writer.write_all(&chunk.buf[..chunk.len]) {
                            report_pipeline_error(
                                &error_disk,
                                &stop_disk,
                                "disk",
                                format!("failed writing raw data: {e}"),
                            );
                            break;
                        }
                        let _ = disk_pool_tx.send(chunk.buf);
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if stop_disk.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }

            if let Err(e) = writer.flush() {
                report_pipeline_error(
                    &error_disk,
                    &stop_disk,
                    "disk",
                    format!("failed flushing output file: {e}"),
                );
            }
        });

        threads.push(disk_thread);
    }

    let width = options.sensor_width;
    let height = options.sensor_height;
    let preview_pool_tx_preview = preview_pool_tx.clone();
    let preview_thread = thread::spawn(move || {
        let mut events = Vec::<CdEvent>::with_capacity(4_096);
        let mut frame_events = Vec::<CdEvent>::with_capacity(8_192);
        let mut frame_buf = vec![0_u16; width as usize * height as usize];
        let mut frame_buf_on = vec![0_u16; width as usize * height as usize];
        let mut frame_buf_off = vec![0_u16; width as usize * height as usize];
        let mut on_count = 0_u64;
        let mut off_count = 0_u64;
        let mut frame_start_ts: Option<u64> = None;

        loop {
            match preview_rx.recv_timeout(Duration::from_millis(2)) {
                Ok(chunk) => {
                    let decode_result = decoder.decode_bytes(&chunk.buf[..chunk.len], &mut events);
                    let _ = preview_pool_tx_preview.try_send(chunk.buf);

                    if let Err(e) = decode_result {
                        report_pipeline_error(
                            &error_preview,
                            &stop_preview,
                            "preview",
                            format!("EVT3 decode failed: {e}"),
                        );
                        break;
                    }
                    if let Ok(mut s) = stats_preview.lock() {
                        s.record_packet(Instant::now(), 0, events.len() as u64);
                    }
                    for ev in &events {
                        if ev.x >= width || ev.y >= height {
                            continue;
                        }
                        let idx = ev.y as usize * width as usize + ev.x as usize;
                        frame_buf[idx] = frame_buf[idx].saturating_add(1);
                        if ev.polarity {
                            frame_buf_on[idx] = frame_buf_on[idx].saturating_add(1);
                            on_count += 1;
                        } else {
                            frame_buf_off[idx] = frame_buf_off[idx].saturating_add(1);
                            off_count += 1;
                        }
                        frame_start_ts.get_or_insert(ev.timestamp);
                    }
                    if raw_events_preview.load(Ordering::Relaxed) {
                        frame_events.extend_from_slice(&events);
                    } else if !frame_events.is_empty() {
                        frame_events.clear();
                    }

                    if let (Some(t0), Some(last_ts)) =
                        (frame_start_ts, events.last().map(|e| e.timestamp))
                    {
                        if last_ts.saturating_sub(t0) >= acq_preview.load(Ordering::Relaxed) {
                            let raw_events = if raw_events_preview.load(Ordering::Relaxed) {
                                let next_capacity = frame_events.capacity().max(8_192);
                                Some(std::mem::replace(
                                    &mut frame_events,
                                    Vec::with_capacity(next_capacity),
                                ))
                            } else {
                                frame_events.clear();
                                None
                            };
                            let frame = PreviewFrame {
                                width,
                                height,
                                pixels: frame_buf.clone(),
                                pixels_on: frame_buf_on.clone(),
                                pixels_off: frame_buf_off.clone(),
                                on_count,
                                off_count,
                                events: raw_events,
                                window_start_us: t0,
                                window_end_us: last_ts,
                            };
                            let _ = frame_tx.try_send(frame);
                            frame_buf.fill(0);
                            frame_buf_on.fill(0);
                            frame_buf_off.fill(0);
                            on_count = 0;
                            off_count = 0;
                            frame_start_ts = None;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if stop_preview.load(Ordering::Relaxed) && preview_rx.is_empty() {
                        break;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Err(err) = decoder.finish_stream() {
            report_pipeline_error(
                &error_preview,
                &stop_preview,
                "preview",
                format!("preview stream finalize failed: {err}"),
            );
        }
    });

    threads.push(preview_thread);

    Ok(PipelineController {
        frame_rx,
        settings_tx,
        acq_time_us,
        raw_events_needed,
        error_rx,
        stop,
        stats,
        threads,
    })
}

fn write_evt3_header_lines(mut writer: impl Write, width: u16, height: u16) -> Result<()> {
    writeln!(writer, "% format EVT3;width={};height={}", width, height)?;
    writeln!(writer, "% geometry {}x{}", width, height)?;
    writeln!(writer, "% evt 3.0")?;
    writeln!(writer, "% end")?;
    Ok(())
}

fn prepare_output_writer(
    output_path: &Path,
    write_evt3_header: bool,
    width: u16,
    height: u16,
) -> Result<BufWriter<File>> {
    let file = File::create(output_path)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
    if write_evt3_header {
        write_evt3_header_lines(&mut writer, width, height)?;
    }
    Ok(writer)
}

fn recording_config_path(raw_path: &Path) -> Option<PathBuf> {
    let stem = raw_path.file_stem()?.to_string_lossy();
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}.toml")))
}

fn report_pipeline_error(
    error_tx: &Sender<String>,
    stop: &AtomicBool,
    worker: &str,
    message: String,
) {
    let _ = error_tx.try_send(format!("{worker}: {message}"));
    stop.store(true, Ordering::Relaxed);
}

fn estimate_evt3_cd_events(bytes: &[u8]) -> u64 {
    let mut events = 0_u64;
    for chunk in bytes.chunks_exact(2) {
        let w = u16::from_le_bytes([chunk[0], chunk[1]]);
        match (w >> 12) & 0xF {
            0x2 => events += 1,                                // ADDR_X
            0x5 => events += (w & 0x00FF).count_ones() as u64, // VECT_8
            0x4 => events += (w & 0x0FFF).count_ones() as u64, // VECT_12
            _ => {}
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words_to_bytes(words: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(words.len() * 2);
        for &w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    #[test]
    fn decodes_single_addr_event_with_evt3_core_adapter() {
        let words = [
            (0x8 << 12) | 0x001,         // TIME_HIGH
            (0x6 << 12) | 0x002,         // TIME_LOW
            20,                          // ADDR_Y
            (0x2 << 12) | (1 << 11) | 9, // ADDR_X, pol=1
        ];
        let bytes = words_to_bytes(&words);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        decoder
            .decode_bytes(&bytes, &mut events)
            .expect("decoder must succeed");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].x, 9);
        assert_eq!(events[0].y, 20);
        assert!(events[0].polarity);
        assert_eq!(events[0].timestamp, (1_u64 << 12) | 2);
    }

    #[test]
    fn decodes_vector_events_with_evt3_core_adapter() {
        let words = [
            (0x8 << 12) | 0x001,       // TIME_HIGH
            (0x6 << 12) | 0x010,       // TIME_LOW
            7,                         // ADDR_Y
            (0x3 << 12) | 100,         // VECT_BASE_X, pol=0
            (0x5 << 12) | 0b0000_0101, // VECT_8 -> x=100 and 102
        ];
        let bytes = words_to_bytes(&words);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        decoder
            .decode_bytes(&bytes, &mut events)
            .expect("decoder must succeed");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].x, 100);
        assert_eq!(events[1].x, 102);
        assert_eq!(events[0].y, 7);
        assert_eq!(events[1].y, 7);
    }

    #[test]
    fn handles_odd_chunk_boundaries_across_calls() {
        let words = [
            (0x8 << 12) | 0x001,
            (0x6 << 12) | 0x003,
            11,
            (0x2 << 12) | 5,
            (0x2 << 12) | (1 << 11) | 6,
        ];
        let bytes = words_to_bytes(&words);
        let split = bytes.len() - 1;
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        let mut all_events = Vec::new();

        decoder
            .decode_bytes(&bytes[..split], &mut events)
            .expect("first chunk must decode");
        all_events.extend(events.iter().copied());
        decoder
            .decode_bytes(&bytes[split..], &mut events)
            .expect("second chunk must decode");
        all_events.extend(events.iter().copied());
        decoder.finish_stream().expect("stream must finalize");

        assert_eq!(all_events.len(), 2);
        assert_eq!(all_events[0].x, 5);
        assert_eq!(all_events[1].x, 6);
        assert_eq!(all_events[0].y, 11);
        assert_eq!(all_events[1].y, 11);
        assert!(!all_events[0].polarity);
        assert!(all_events[1].polarity);
    }

    #[test]
    fn finish_stream_ignores_trailing_half_word() {
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();

        decoder
            .decode_bytes(&[0x34], &mut events)
            .expect("decoder must accept partial byte stream");
        decoder
            .finish_stream()
            .expect("dangling half word must be ignored for replay EOF");
    }

    #[test]
    fn finish_stream_succeeds_on_even_boundary() {
        let words = [(0x8 << 12) | 0x001, (0x6 << 12) | 0x002];
        let bytes = words_to_bytes(&words);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();

        decoder
            .decode_bytes(&bytes, &mut events)
            .expect("decoder must succeed");
        decoder.finish_stream().expect("stream must finalize");
    }

    #[test]
    fn estimates_evt3_event_count() {
        let words = [
            (0x2 << 12) | 1,                // 1x ADDR_X
            (0x5 << 12) | 0b0001_0011,      // 3x VECT_8
            (0x4 << 12) | 0b0000_0010_0001, // 2x VECT_12
        ];
        let bytes = words_to_bytes(&words);
        assert_eq!(estimate_evt3_cd_events(&bytes), 6);
    }

    #[test]
    fn snapshot_uses_recent_window_rate() {
        let start = Instant::now();
        let mut stats = PipelineStatsInner {
            started: start,
            bytes_total: 0,
            events_total: 0,
            recent_samples: VecDeque::new(),
        };

        stats.record_packet(start, 1_048_576, 1_000_000);
        stats.record_packet(start + Duration::from_millis(250), 1_048_576, 1_000_000);

        let snapshot = stats.snapshot_at(start + Duration::from_millis(500));

        assert_eq!(snapshot.bytes_total, 2_097_152);
        assert_eq!(snapshot.events_total, 2_000_000);
        assert!((snapshot.mb_per_s - 4.0).abs() < 0.05);
        assert!((snapshot.mev_per_s - 4.0).abs() < 0.05);
    }

    #[test]
    fn snapshot_drops_stale_samples_from_current_rate() {
        let start = Instant::now();
        let mut stats = PipelineStatsInner {
            started: start,
            bytes_total: 0,
            events_total: 0,
            recent_samples: VecDeque::new(),
        };

        stats.record_packet(start, 1_048_576, 1_000_000);
        stats.record_packet(start + Duration::from_millis(1_100), 524_288, 500_000);

        let snapshot = stats.snapshot_at(start + Duration::from_millis(1_200));

        assert_eq!(snapshot.bytes_total, 1_572_864);
        assert_eq!(snapshot.events_total, 1_500_000);
        assert!((snapshot.mb_per_s - 0.5).abs() < 0.05);
        assert!((snapshot.mev_per_s - 0.5).abs() < 0.05);
    }
}
