use std::{
    collections::VecDeque,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use crossbeam_channel::{
    bounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender, TryRecvError, TrySendError,
};
use evt3_core::{CdEvent as Evt3CdEvent, Evt3Decoder, TriggerEvent as Evt3TriggerEvent};

use crate::{
    camera::PacketStreamCamera,
    config::CameraConfig,
    evt3_timestamps::Evt3TimestampUnwrapper,
    metadata::{RecordingMetadata, RecordingSidecar},
    CameraError, Result,
};

pub const BUF_SIZE: usize = 65_536;
pub const RAW_BUFFER_POOL_CAPACITY: usize = 8;
pub const DISK_QUEUE_CAPACITY: usize = 8;
pub const PREVIEW_PACKET_POOL_CAPACITY: usize = 4;
pub const PREVIEW_PACKET_QUEUE_CAPACITY: usize = 4;
pub const PREVIEW_FRAME_QUEUE_CAPACITY: usize = 4;
const PREVIEW_FRAME_POOL_CAPACITY: usize = PREVIEW_FRAME_QUEUE_CAPACITY * 2;
const DEFAULT_DISK_WRITER_BUFFER_BYTES: usize = 4 * 1024 * 1024;
pub const N_BUFFERS: usize = RAW_BUFFER_POOL_CAPACITY;
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
    timestamp_unwrapper: Evt3TimestampUnwrapper,
}

impl Evt3CorePreviewDecoder {
    pub fn with_expected_timestamp(expected_timestamp_us: u64) -> Self {
        Self {
            timestamp_unwrapper: Evt3TimestampUnwrapper::with_expected_timestamp(
                expected_timestamp_us,
            ),
            ..Self::default()
        }
    }
}

impl PreviewDecoder for Evt3CorePreviewDecoder {
    fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()> {
        out.clear();
        self.cd_scratch.clear();
        self.trigger_scratch.clear();

        self.inner
            .decode_bytes(bytes, &mut self.cd_scratch, &mut self.trigger_scratch)
            .map_err(|e| CameraError::Other(format!("evt3 decode failed: {e}")))?;

        out.reserve(self.cd_scratch.len());
        for event in &self.cd_scratch {
            out.push(CdEvent {
                x: event.x,
                y: event.y,
                timestamp: self.timestamp_unwrapper.map_timestamp(event.timestamp),
                polarity: event.polarity != 0,
            });
        }

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
    pub cached_total_histogram: Vec<u64>,
    pub cached_signed_histogram: Vec<u64>,
    pub on_count: u64,
    pub off_count: u64,
    pub events: Option<Vec<CdEvent>>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

impl Drop for PreviewFrame {
    fn drop(&mut self) {
        recycle_preview_frame_buffers(PreviewFrameBuffers {
            pixels: std::mem::take(&mut self.pixels),
            pixels_on: std::mem::take(&mut self.pixels_on),
            pixels_off: std::mem::take(&mut self.pixels_off),
            total_histogram: std::mem::take(&mut self.cached_total_histogram),
            signed_histogram: std::mem::take(&mut self.cached_signed_histogram),
        });
    }
}

pub const PREVIEW_HISTOGRAM_BINS: usize = 4096;

#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub output_path: Option<PathBuf>,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub write_evt3_header: bool,
    pub disk_writer_buffer_bytes: usize,
    pub metadata: Option<RecordingMetadata>,
}

impl PipelineOptions {
    pub fn new(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: Some(output_path.into()),
            sensor_width: 1280,
            sensor_height: 720,
            write_evt3_header: true,
            disk_writer_buffer_bytes: DEFAULT_DISK_WRITER_BUFFER_BYTES,
            metadata: None,
        }
    }

    pub fn preview_only(width: u16, height: u16) -> Self {
        Self {
            output_path: None,
            sensor_width: width,
            sensor_height: height,
            write_evt3_header: false,
            disk_writer_buffer_bytes: DEFAULT_DISK_WRITER_BUFFER_BYTES,
            metadata: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PipelineStatsSnapshot {
    pub elapsed_s: f64,
    pub bytes_total: u64,
    pub events_total: u64,
    pub recording_duration_us: Option<u64>,
    pub mb_per_s: f64,
    pub mev_per_s: f64,
    pub preview_packet_drops: u64,
    pub preview_packet_queue_high_water: usize,
    pub preview_frame_drops: u64,
    pub preview_frame_queue_high_water: usize,
    pub disk_queue_high_water: usize,
    pub disk_send_wait_us: u64,
    pub disk_write_us: u64,
    pub preview_packets_processed: u64,
    pub preview_frames_emitted: u64,
    pub preview_decode_us: u64,
    pub preview_accumulate_us: u64,
    pub preview_raw_event_copy_us: u64,
    pub preview_frame_send_us: u64,
}

impl PipelineStatsSnapshot {
    pub fn preview_decode_avg_ms(self) -> f64 {
        avg_stage_ms(self.preview_decode_us, self.preview_packets_processed)
    }

    pub fn preview_accumulate_avg_ms(self) -> f64 {
        avg_stage_ms(self.preview_accumulate_us, self.preview_packets_processed)
    }

    pub fn preview_raw_event_copy_avg_ms(self) -> f64 {
        avg_stage_ms(
            self.preview_raw_event_copy_us,
            self.preview_packets_processed,
        )
    }

    pub fn preview_frame_send_avg_ms(self) -> f64 {
        avg_stage_ms(self.preview_frame_send_us, self.preview_frames_emitted)
    }
}

fn avg_stage_ms(total_us: u64, samples: u64) -> f64 {
    if samples == 0 {
        0.0
    } else {
        total_us as f64 / samples as f64 / 1_000.0
    }
}

#[derive(Debug)]
struct PipelineStatsInner {
    started: Instant,
    bytes_total: u64,
    events_total: u64,
    first_timestamp_us: Option<u64>,
    last_timestamp_us: Option<u64>,
    preview_packet_drops: u64,
    preview_packet_queue_high_water: usize,
    preview_frame_drops: u64,
    preview_frame_queue_high_water: usize,
    disk_queue_high_water: usize,
    disk_send_wait_us: u64,
    disk_write_us: u64,
    preview_packets_processed: u64,
    preview_frames_emitted: u64,
    preview_decode_us: u64,
    preview_accumulate_us: u64,
    preview_raw_event_copy_us: u64,
    preview_frame_send_us: u64,
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
            first_timestamp_us: None,
            last_timestamp_us: None,
            preview_packet_drops: 0,
            preview_packet_queue_high_water: 0,
            preview_frame_drops: 0,
            preview_frame_queue_high_water: 0,
            disk_queue_high_water: 0,
            disk_send_wait_us: 0,
            disk_write_us: 0,
            preview_packets_processed: 0,
            preview_frames_emitted: 0,
            preview_decode_us: 0,
            preview_accumulate_us: 0,
            preview_raw_event_copy_us: 0,
            preview_frame_send_us: 0,
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
            recording_duration_us: self
                .last_timestamp_us
                .zip(self.first_timestamp_us)
                .and_then(|(last, first)| last.checked_sub(first)),
            mb_per_s: recent_bytes as f64 / current_window_s / (1024.0 * 1024.0),
            mev_per_s: recent_events as f64 / current_window_s / 1_000_000.0,
            preview_packet_drops: self.preview_packet_drops,
            preview_packet_queue_high_water: self.preview_packet_queue_high_water,
            preview_frame_drops: self.preview_frame_drops,
            preview_frame_queue_high_water: self.preview_frame_queue_high_water,
            disk_queue_high_water: self.disk_queue_high_water,
            disk_send_wait_us: self.disk_send_wait_us,
            disk_write_us: self.disk_write_us,
            preview_packets_processed: self.preview_packets_processed,
            preview_frames_emitted: self.preview_frames_emitted,
            preview_decode_us: self.preview_decode_us,
            preview_accumulate_us: self.preview_accumulate_us,
            preview_raw_event_copy_us: self.preview_raw_event_copy_us,
            preview_frame_send_us: self.preview_frame_send_us,
        }
    }

    fn prune_recent(&mut self, now: Instant) {
        let cutoff = now.checked_sub(CURRENT_RATE_WINDOW).unwrap_or(self.started);
        while matches!(self.recent_samples.front(), Some(sample) if sample.at < cutoff) {
            self.recent_samples.pop_front();
        }
    }

    fn record_preview_packet_drop(&mut self) {
        self.preview_packet_drops = self.preview_packet_drops.saturating_add(1);
    }

    fn record_preview_packet_queue_depth(&mut self, queue_depth: usize) {
        self.preview_packet_queue_high_water =
            self.preview_packet_queue_high_water.max(queue_depth);
    }

    fn record_event_timestamps(&mut self, first_timestamp_us: u64, last_timestamp_us: u64) {
        self.first_timestamp_us.get_or_insert(first_timestamp_us);
        self.last_timestamp_us = Some(last_timestamp_us);
    }

    fn record_preview_frame_drop(&mut self) {
        self.preview_frame_drops = self.preview_frame_drops.saturating_add(1);
    }

    fn record_preview_frame_queue_depth(&mut self, queue_depth: usize) {
        self.preview_frame_queue_high_water = self.preview_frame_queue_high_water.max(queue_depth);
    }

    fn record_disk_queue_depth(&mut self, queue_depth: usize) {
        self.disk_queue_high_water = self.disk_queue_high_water.max(queue_depth);
    }

    fn record_disk_send_wait(&mut self, wait: Duration) {
        self.disk_send_wait_us = self
            .disk_send_wait_us
            .saturating_add(wait.as_micros() as u64);
    }

    fn record_disk_write_time(&mut self, write_time: Duration) {
        self.disk_write_us = self
            .disk_write_us
            .saturating_add(write_time.as_micros() as u64);
    }

    fn record_preview_decode_time(&mut self, duration: Duration) {
        self.preview_packets_processed = self.preview_packets_processed.saturating_add(1);
        self.preview_decode_us = self
            .preview_decode_us
            .saturating_add(duration.as_micros() as u64);
    }

    fn record_preview_accumulate_time(&mut self, duration: Duration) {
        self.preview_accumulate_us = self
            .preview_accumulate_us
            .saturating_add(duration.as_micros() as u64);
    }

    fn record_preview_raw_event_copy_time(&mut self, duration: Duration) {
        self.preview_raw_event_copy_us = self
            .preview_raw_event_copy_us
            .saturating_add(duration.as_micros() as u64);
    }

    fn record_preview_frame_send_time(&mut self, duration: Duration) {
        self.preview_frames_emitted = self.preview_frames_emitted.saturating_add(1);
        self.preview_frame_send_us = self
            .preview_frame_send_us
            .saturating_add(duration.as_micros() as u64);
    }
}

type UsbBuffer = Box<[u8; BUF_SIZE]>;

#[derive(Debug)]
struct PreviewFrameBuffers {
    pixels: Vec<u16>,
    pixels_on: Vec<u16>,
    pixels_off: Vec<u16>,
    total_histogram: Vec<u64>,
    signed_histogram: Vec<u64>,
}

static PREVIEW_FRAME_BUFFER_POOL: OnceLock<Mutex<Vec<PreviewFrameBuffers>>> = OnceLock::new();

fn preview_frame_buffer_pool() -> &'static Mutex<Vec<PreviewFrameBuffers>> {
    PREVIEW_FRAME_BUFFER_POOL
        .get_or_init(|| Mutex::new(Vec::with_capacity(PREVIEW_FRAME_POOL_CAPACITY)))
}

fn take_preview_frame_buffers(pixel_count: usize) -> PreviewFrameBuffers {
    let mut buffers = preview_frame_buffer_pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(mut frame_buffers) = buffers.pop() {
        frame_buffers.pixels.clear();
        frame_buffers.pixels.resize(pixel_count, 0);
        frame_buffers.pixels_on.clear();
        frame_buffers.pixels_on.resize(pixel_count, 0);
        frame_buffers.pixels_off.clear();
        frame_buffers.pixels_off.resize(pixel_count, 0);
        reset_histogram_bins(&mut frame_buffers.total_histogram, pixel_count as u64);
        reset_histogram_bins(&mut frame_buffers.signed_histogram, pixel_count as u64);
        frame_buffers
    } else {
        PreviewFrameBuffers {
            pixels: vec![0_u16; pixel_count],
            pixels_on: vec![0_u16; pixel_count],
            pixels_off: vec![0_u16; pixel_count],
            total_histogram: histogram_bins_with_zero_count(pixel_count as u64),
            signed_histogram: histogram_bins_with_zero_count(pixel_count as u64),
        }
    }
}

fn recycle_preview_frame_buffers(buffers: PreviewFrameBuffers) {
    let mut pool = preview_frame_buffer_pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if pool.len() < PREVIEW_FRAME_POOL_CAPACITY {
        pool.push(buffers);
    }
}

fn reset_preview_frame_accumulators(
    frame_buffers: &mut PreviewFrameBuffers,
    frame_events: &mut Vec<CdEvent>,
    on_count: &mut u64,
    off_count: &mut u64,
    frame_start_ts: &mut Option<u64>,
) {
    frame_buffers.pixels.fill(0);
    frame_buffers.pixels_on.fill(0);
    frame_buffers.pixels_off.fill(0);
    let pixel_count = frame_buffers.pixels.len() as u64;
    reset_histogram_bins(&mut frame_buffers.total_histogram, pixel_count);
    reset_histogram_bins(&mut frame_buffers.signed_histogram, pixel_count);
    frame_events.clear();
    *on_count = 0;
    *off_count = 0;
    *frame_start_ts = None;
}

fn histogram_index(value: u16) -> usize {
    usize::from(value).min(PREVIEW_HISTOGRAM_BINS - 1)
}

fn histogram_bins_with_zero_count(pixel_count: u64) -> Vec<u64> {
    let mut histogram = vec![0_u64; PREVIEW_HISTOGRAM_BINS];
    histogram[0] = pixel_count;
    histogram
}

fn reset_histogram_bins(histogram: &mut Vec<u64>, pixel_count: u64) {
    histogram.clear();
    histogram.resize(PREVIEW_HISTOGRAM_BINS, 0);
    histogram[0] = pixel_count;
}

fn transition_histogram_bin(histogram: &mut [u64], old_value: u16, new_value: u16) {
    let old_index = histogram_index(old_value);
    let new_index = histogram_index(new_value);
    histogram[old_index] = histogram[old_index].saturating_sub(1);
    histogram[new_index] = histogram[new_index].saturating_add(1);
}

#[cfg(test)]
fn preview_frame_pool_len() -> usize {
    preview_frame_buffer_pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .len()
}

#[cfg(test)]
fn clear_preview_frame_pool() {
    preview_frame_buffer_pool()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

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
    recording_sidecar: Option<RecordingSidecarState>,
    threads: Vec<thread::JoinHandle<()>>,
}

struct RecordingSidecarState {
    path: PathBuf,
    config: CameraConfig,
    metadata: RecordingMetadata,
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
        let stats_snapshot = self.stats_snapshot();
        if let Some(recording_sidecar) = &mut self.recording_sidecar {
            recording_sidecar.metadata.update_timing(
                stats_snapshot.recording_duration_us,
                stats_snapshot.events_total,
            );
            RecordingSidecar::new(
                recording_sidecar.config.clone(),
                recording_sidecar.metadata.clone(),
            )
            .save_to_path(&recording_sidecar.path)?;
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

    let PipelineOptions {
        output_path,
        sensor_width,
        sensor_height,
        write_evt3_header,
        disk_writer_buffer_bytes,
        metadata,
    } = options;
    let recording = output_path.is_some();
    let mut recording_sidecar = None;
    let recording_metadata = metadata.unwrap_or_default();

    if let Some(ref output_path) = output_path {
        if let Some(config_path) = recording_config_path(output_path) {
            RecordingSidecar::new(initial_config.clone(), recording_metadata.clone())
                .save_to_path(&config_path)?;
            recording_sidecar = Some(RecordingSidecarState {
                path: config_path,
                config: initial_config.clone(),
                metadata: recording_metadata.clone(),
            });
        }
    }

    let disk_writer = if let Some(ref output_path) = output_path {
        Some(prepare_output_writer(
            output_path,
            write_evt3_header,
            sensor_width,
            sensor_height,
            disk_writer_buffer_bytes,
            Some(&recording_metadata),
        )?)
    } else {
        None
    };

    let (pool_tx, pool_rx) = bounded::<UsbBuffer>(RAW_BUFFER_POOL_CAPACITY);
    for _ in 0..RAW_BUFFER_POOL_CAPACITY {
        pool_tx
            .send(Box::new([0_u8; BUF_SIZE]))
            .map_err(|e| CameraError::Channel(format!("buffer pool init failed: {e}")))?;
    }
    let (preview_pool_tx, preview_pool_rx) = bounded::<UsbBuffer>(PREVIEW_PACKET_POOL_CAPACITY);
    for _ in 0..PREVIEW_PACKET_POOL_CAPACITY {
        preview_pool_tx
            .send(Box::new([0_u8; BUF_SIZE]))
            .map_err(|e| CameraError::Channel(format!("preview pool init failed: {e}")))?;
    }

    let (disk_tx, disk_rx) = if recording {
        let (tx, rx) = bounded::<DiskChunk>(DISK_QUEUE_CAPACITY);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };
    let (preview_tx, preview_rx) = bounded::<PreviewChunk>(PREVIEW_PACKET_QUEUE_CAPACITY);
    let (frame_tx, frame_rx) = bounded::<PreviewFrame>(PREVIEW_FRAME_QUEUE_CAPACITY);
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
            // PacketStreamCamera does not currently expose upstream overflow counters.
            // Surface them here once the transport API makes them available.

            if !preview_tx.is_full() {
                match preview_pool_rx.try_recv() {
                    Ok(mut preview_buf) => {
                        preview_buf[..len].copy_from_slice(&buf[..len]);
                        match preview_tx.try_send(PreviewChunk {
                            buf: preview_buf,
                            len,
                        }) {
                            Ok(()) => {
                                if let Ok(mut s) = stats_usb.lock() {
                                    s.record_preview_packet_queue_depth(preview_tx.len());
                                }
                            }
                            Err(TrySendError::Full(chunk))
                            | Err(TrySendError::Disconnected(chunk)) => {
                                if let Ok(mut s) = stats_usb.lock() {
                                    s.record_preview_packet_drop();
                                }
                                let _ = preview_pool_tx_usb.try_send(chunk.buf);
                            }
                        }
                    }
                    Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {
                        if let Ok(mut s) = stats_usb.lock() {
                            s.record_preview_packet_drop();
                        }
                    }
                }
            } else if let Ok(mut s) = stats_usb.lock() {
                s.record_preview_packet_drop();
            }

            if let Some(ref disk_tx) = disk_tx {
                let disk_send_started = Instant::now();
                match disk_tx.send_timeout(DiskChunk { buf, len }, Duration::from_millis(50)) {
                    Ok(_) => {
                        if let Ok(mut s) = stats_usb.lock() {
                            s.record_disk_send_wait(disk_send_started.elapsed());
                            s.record_disk_queue_depth(disk_tx.len());
                        }
                    }
                    Err(SendTimeoutError::Timeout(chunk)) => match disk_tx.send(chunk) {
                        Ok(_) => {
                            if let Ok(mut s) = stats_usb.lock() {
                                s.record_disk_send_wait(disk_send_started.elapsed());
                                s.record_disk_queue_depth(disk_tx.len());
                            }
                        }
                        Err(e) => {
                            if let Ok(mut s) = stats_usb.lock() {
                                s.record_disk_send_wait(disk_send_started.elapsed());
                            }
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
                        if let Ok(mut s) = stats_usb.lock() {
                            s.record_disk_send_wait(disk_send_started.elapsed());
                        }
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
        let stats_disk = Arc::clone(&stats);

        let disk_thread = thread::spawn(move || {
            loop {
                match disk_rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(chunk) => {
                        let write_started = Instant::now();
                        let write_result = writer.write_all(&chunk.buf[..chunk.len]);
                        if let Ok(mut s) = stats_disk.lock() {
                            s.record_disk_write_time(write_started.elapsed());
                        }
                        if let Err(e) = write_result {
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

            let flush_started = Instant::now();
            let flush_result = writer.flush();
            if let Ok(mut s) = stats_disk.lock() {
                s.record_disk_write_time(flush_started.elapsed());
            }
            if let Err(e) = flush_result {
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

    let width = sensor_width;
    let height = sensor_height;
    let pixel_count = width as usize * height as usize;
    let preview_pool_tx_preview = preview_pool_tx.clone();
    let preview_thread = thread::spawn(move || {
        let mut events = Vec::<CdEvent>::with_capacity(4_096);
        let mut frame_events = Vec::<CdEvent>::with_capacity(8_192);
        let mut frame_buffers = take_preview_frame_buffers(pixel_count);
        let mut on_count = 0_u64;
        let mut off_count = 0_u64;
        let mut frame_start_ts: Option<u64> = None;

        loop {
            match preview_rx.recv_timeout(Duration::from_millis(2)) {
                Ok(chunk) => {
                    let decode_started = Instant::now();
                    let decode_result = decoder.decode_bytes(&chunk.buf[..chunk.len], &mut events);
                    let decode_elapsed = decode_started.elapsed();
                    let _ = preview_pool_tx_preview.try_send(chunk.buf);

                    if let Ok(mut s) = stats_preview.lock() {
                        s.record_preview_decode_time(decode_elapsed);
                    }
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
                        if let (Some(first), Some(last)) = (
                            events.first().map(|event| event.timestamp),
                            events.last().map(|event| event.timestamp),
                        ) {
                            s.record_event_timestamps(first, last);
                        }
                    }
                    let accumulate_started = Instant::now();
                    for ev in &events {
                        if ev.x >= width || ev.y >= height {
                            continue;
                        }
                        let idx = ev.y as usize * width as usize + ev.x as usize;
                        let old_total = frame_buffers.pixels[idx];
                        let new_total = old_total.saturating_add(1);
                        frame_buffers.pixels[idx] = new_total;
                        transition_histogram_bin(
                            &mut frame_buffers.total_histogram,
                            old_total,
                            new_total,
                        );
                        let old_on = frame_buffers.pixels_on[idx];
                        let old_off = frame_buffers.pixels_off[idx];
                        if ev.polarity {
                            frame_buffers.pixels_on[idx] = old_on.saturating_add(1);
                            on_count += 1;
                        } else {
                            frame_buffers.pixels_off[idx] = old_off.saturating_add(1);
                            off_count += 1;
                        }
                        let new_on = frame_buffers.pixels_on[idx];
                        let new_off = frame_buffers.pixels_off[idx];
                        transition_histogram_bin(
                            &mut frame_buffers.signed_histogram,
                            old_on.abs_diff(old_off),
                            new_on.abs_diff(new_off),
                        );
                        frame_start_ts.get_or_insert(ev.timestamp);
                    }
                    let accumulate_elapsed = accumulate_started.elapsed();
                    let mut raw_event_copy_elapsed = Duration::ZERO;
                    if raw_events_preview.load(Ordering::Relaxed) {
                        let raw_event_copy_started = Instant::now();
                        frame_events.extend_from_slice(&events);
                        raw_event_copy_elapsed = raw_event_copy_started.elapsed();
                    } else if !frame_events.is_empty() {
                        frame_events.clear();
                    }
                    if let Ok(mut s) = stats_preview.lock() {
                        s.record_preview_accumulate_time(accumulate_elapsed);
                        if raw_event_copy_elapsed > Duration::ZERO {
                            s.record_preview_raw_event_copy_time(raw_event_copy_elapsed);
                        }
                    }

                    if let (Some(t0), Some(last_ts)) =
                        (frame_start_ts, events.last().map(|e| e.timestamp))
                    {
                        if last_ts.saturating_sub(t0) >= acq_preview.load(Ordering::Relaxed) {
                            if frame_tx.is_full() {
                                reset_preview_frame_accumulators(
                                    &mut frame_buffers,
                                    &mut frame_events,
                                    &mut on_count,
                                    &mut off_count,
                                    &mut frame_start_ts,
                                );
                                if let Ok(mut s) = stats_preview.lock() {
                                    s.record_preview_frame_drop();
                                }
                                continue;
                            }
                            let frame_send_started = Instant::now();
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
                            let frame_buffers = std::mem::replace(
                                &mut frame_buffers,
                                take_preview_frame_buffers(pixel_count),
                            );
                            let frame = PreviewFrame {
                                width,
                                height,
                                pixels: frame_buffers.pixels,
                                pixels_on: frame_buffers.pixels_on,
                                pixels_off: frame_buffers.pixels_off,
                                cached_total_histogram: frame_buffers.total_histogram,
                                cached_signed_histogram: frame_buffers.signed_histogram,
                                on_count,
                                off_count,
                                events: raw_events,
                                window_start_us: t0,
                                window_end_us: last_ts,
                            };
                            match frame_tx.try_send(frame) {
                                Ok(()) => {
                                    if let Ok(mut s) = stats_preview.lock() {
                                        s.record_preview_frame_queue_depth(frame_tx.len());
                                        s.record_preview_frame_send_time(
                                            frame_send_started.elapsed(),
                                        );
                                    }
                                }
                                Err(err) => {
                                    if let Ok(mut s) = stats_preview.lock() {
                                        s.record_preview_frame_drop();
                                    }
                                    drop(err);
                                }
                            }
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
        recording_sidecar,
        threads,
    })
}

fn write_evt3_header_lines(
    mut writer: impl Write,
    width: u16,
    height: u16,
    metadata: Option<&RecordingMetadata>,
) -> Result<()> {
    writeln!(writer, "% format EVT3;width={};height={}", width, height)?;
    writeln!(writer, "% geometry {}x{}", width, height)?;
    writeln!(writer, "% evt 3.0")?;
    if let Some(metadata) = metadata {
        for line in metadata.to_header_lines() {
            writeln!(writer, "{line}")?;
        }
    }
    writeln!(writer, "% end")?;
    Ok(())
}

fn prepare_output_writer(
    output_path: &Path,
    write_evt3_header: bool,
    width: u16,
    height: u16,
    disk_writer_buffer_bytes: usize,
    metadata: Option<&RecordingMetadata>,
) -> Result<BufWriter<File>> {
    let file = File::create(output_path)?;
    let mut writer = BufWriter::with_capacity(disk_writer_buffer_bytes.max(1), file);
    if write_evt3_header {
        write_evt3_header_lines(&mut writer, width, height, metadata)?;
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
    use crate::metadata::RecordingMetadata;

    fn words_to_bytes(words: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(words.len() * 2);
        for &w in words {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    fn test_stats(start: Instant) -> PipelineStatsInner {
        PipelineStatsInner {
            started: start,
            bytes_total: 0,
            events_total: 0,
            first_timestamp_us: None,
            last_timestamp_us: None,
            preview_packet_drops: 0,
            preview_packet_queue_high_water: 0,
            preview_frame_drops: 0,
            preview_frame_queue_high_water: 0,
            disk_queue_high_water: 0,
            disk_send_wait_us: 0,
            disk_write_us: 0,
            preview_packets_processed: 0,
            preview_frames_emitted: 0,
            preview_decode_us: 0,
            preview_accumulate_us: 0,
            preview_raw_event_copy_us: 0,
            preview_frame_send_us: 0,
            recent_samples: VecDeque::new(),
        }
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
    fn unwraps_evt3_timestamps_across_rollover() {
        let first_chunk =
            words_to_bytes(&[(0x8 << 12) | 0xFFF, (0x6 << 12) | 0xFF0, 8, (0x2 << 12) | 4]);
        let second_chunk = words_to_bytes(&[(0x8 << 12), (0x6 << 12) | 0x010, 8, (0x2 << 12) | 5]);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        let mut all_events = Vec::new();

        decoder
            .decode_bytes(&first_chunk, &mut events)
            .expect("first chunk must decode");
        all_events.extend(events.iter().copied());
        decoder
            .decode_bytes(&second_chunk, &mut events)
            .expect("second chunk must decode");
        all_events.extend(events.iter().copied());

        assert_eq!(all_events.len(), 2);
        assert_eq!(
            all_events[0].timestamp,
            crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US - 16
        );
        assert_eq!(
            all_events[1].timestamp,
            crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US + 16
        );
    }

    #[test]
    fn seeds_evt3_timestamp_epoch_for_mid_file_decode() {
        let bytes = words_to_bytes(&[(0x8 << 12), (0x6 << 12) | 0x010, 8, (0x2 << 12) | 5]);
        let mut decoder = Evt3CorePreviewDecoder::with_expected_timestamp(
            crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US,
        );
        let mut events = Vec::new();

        decoder
            .decode_bytes(&bytes, &mut events)
            .expect("seeded chunk must decode");

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].timestamp,
            crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US + 16
        );
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
        let mut stats = test_stats(start);

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
        let mut stats = test_stats(start);

        stats.record_packet(start, 1_048_576, 1_000_000);
        stats.record_packet(start + Duration::from_millis(1_100), 524_288, 500_000);

        let snapshot = stats.snapshot_at(start + Duration::from_millis(1_200));

        assert_eq!(snapshot.bytes_total, 1_572_864);
        assert_eq!(snapshot.events_total, 1_500_000);
        assert!((snapshot.mb_per_s - 0.5).abs() < 0.05);
        assert!((snapshot.mev_per_s - 0.5).abs() < 0.05);
    }

    #[test]
    fn snapshot_includes_preview_and_disk_telemetry() {
        let start = Instant::now();
        let mut stats = test_stats(start);

        stats.record_preview_packet_drop();
        stats.record_preview_packet_drop();
        stats.record_preview_packet_queue_depth(3);
        stats.record_preview_packet_queue_depth(2);
        stats.record_preview_frame_drop();
        stats.record_preview_frame_queue_depth(4);
        stats.record_disk_queue_depth(5);
        stats.record_disk_queue_depth(4);
        stats.record_disk_send_wait(Duration::from_micros(120));
        stats.record_disk_write_time(Duration::from_micros(340));

        let snapshot = stats.snapshot_at(start + Duration::from_millis(10));

        assert_eq!(snapshot.preview_packet_drops, 2);
        assert_eq!(snapshot.preview_packet_queue_high_water, 3);
        assert_eq!(snapshot.preview_frame_drops, 1);
        assert_eq!(snapshot.preview_frame_queue_high_water, 4);
        assert_eq!(snapshot.disk_queue_high_water, 5);
        assert_eq!(snapshot.disk_send_wait_us, 120);
        assert_eq!(snapshot.disk_write_us, 340);
    }

    #[test]
    fn snapshot_reports_preview_thread_stage_averages() {
        let start = Instant::now();
        let mut stats = test_stats(start);

        stats.record_preview_decode_time(Duration::from_micros(300));
        stats.record_preview_decode_time(Duration::from_micros(500));
        stats.record_preview_accumulate_time(Duration::from_micros(700));
        stats.record_preview_accumulate_time(Duration::from_micros(900));
        stats.record_preview_raw_event_copy_time(Duration::from_micros(200));
        stats.record_preview_raw_event_copy_time(Duration::from_micros(400));
        stats.record_preview_frame_send_time(Duration::from_micros(1_200));
        stats.record_preview_frame_send_time(Duration::from_micros(1_800));

        let snapshot = stats.snapshot_at(start + Duration::from_millis(10));

        assert_eq!(snapshot.preview_packets_processed, 2);
        assert_eq!(snapshot.preview_frames_emitted, 2);
        assert_eq!(snapshot.preview_decode_us, 800);
        assert_eq!(snapshot.preview_accumulate_us, 1_600);
        assert_eq!(snapshot.preview_raw_event_copy_us, 600);
        assert_eq!(snapshot.preview_frame_send_us, 3_000);
        assert!((snapshot.preview_decode_avg_ms() - 0.4).abs() < 1e-6);
        assert!((snapshot.preview_accumulate_avg_ms() - 0.8).abs() < 1e-6);
        assert!((snapshot.preview_raw_event_copy_avg_ms() - 0.3).abs() < 1e-6);
        assert!((snapshot.preview_frame_send_avg_ms() - 1.5).abs() < 1e-6);
    }

    #[test]
    fn snapshot_reports_recording_duration_from_timestamps() {
        let start = Instant::now();
        let mut stats = test_stats(start);
        stats.record_event_timestamps(1_000, 26_000);

        let snapshot = stats.snapshot_at(start + Duration::from_millis(10));
        assert_eq!(snapshot.recording_duration_us, Some(25_000));
    }

    #[test]
    fn evt3_header_writer_includes_metadata_lines() {
        let metadata = RecordingMetadata {
            system_id: Some("Prophesee EVK4".into()),
            serial_number: Some("00a1b2c3d4e5f678".into()),
            pixel_pitch_nm: Some(4_860.0),
            ..RecordingMetadata::default()
        };
        let mut encoded = Vec::new();

        write_evt3_header_lines(&mut encoded, 1280, 720, Some(&metadata))
            .expect("header must encode");
        let header = String::from_utf8(encoded).expect("header must stay utf8");

        assert!(header.contains("% format EVT3;width=1280;height=720"));
        assert!(header.contains("% system_id Prophesee EVK4"));
        assert!(header.contains("% serial_number 00a1b2c3d4e5f678"));
        assert!(header.contains("% pixel_pitch_nm 4860"));
        assert!(header.ends_with("% end\n"));
    }

    #[test]
    fn dropped_preview_frames_recycle_buffers() {
        clear_preview_frame_pool();
        assert_eq!(preview_frame_pool_len(), 0);

        let frame = PreviewFrame {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 4],
            pixels_on: vec![5, 6, 7, 8],
            pixels_off: vec![9, 10, 11, 12],
            cached_total_histogram: histogram_bins_with_zero_count(4),
            cached_signed_histogram: histogram_bins_with_zero_count(4),
            on_count: 2,
            off_count: 2,
            events: None,
            window_start_us: 10,
            window_end_us: 20,
        };

        drop(frame);

        assert_eq!(preview_frame_pool_len(), 1);

        let buffers = take_preview_frame_buffers(4);
        assert_eq!(buffers.pixels.len(), 4);
        assert_eq!(buffers.pixels_on.len(), 4);
        assert_eq!(buffers.pixels_off.len(), 4);
        assert_eq!(buffers.total_histogram.len(), PREVIEW_HISTOGRAM_BINS);
        assert_eq!(buffers.signed_histogram.len(), PREVIEW_HISTOGRAM_BINS);
        assert_eq!(preview_frame_pool_len(), 0);
    }
}
