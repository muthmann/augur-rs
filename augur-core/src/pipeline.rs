use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{BufWriter, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};

use augur_event_types::{
    BackpressureBehavior, CompactEvent, ConsumerCursor, CursorId, CursorPolicy, EventChunk,
    EventRing, EventSource, ExternalTriggerEvent, FetchError, FrameWindowEntry, RingAppendError,
};
use crossbeam_channel::{
    bounded, Receiver, RecvTimeoutError, SendTimeoutError, Sender, TryRecvError, TrySendError,
};
use evt3_core::{CdEvent as Evt3CdEvent, Evt3Decoder, TriggerEvent as Evt3TriggerEvent};

use crate::{
    camera::{
        EventCamera, PacketStreamCamera, PacketStreamReader, SensorMonitoring,
        SensorMonitoringSelection,
    },
    config::CameraConfig,
    evt3_timestamps::{Evt3TimestampUnwrapper, SecondaryTimestampMapper},
    metadata::{RecordingMetadata, RecordingSidecar},
    CameraError, Result,
};

pub const BUF_SIZE: usize = 65_536;
// The pool + disk queue bound the bytes that can sit between the USB reader
// and the disk writer. This budget is what rides out disk-write hiccups: once
// the pool is empty the USB thread stops reading and the camera FIFO
// overflows, which shows up as a hard gap in the recording. 256 x 64 KiB
// = 16 MiB, roughly 80 ms of headroom at EVK4 peak ingress (~200 MB/s).
pub const RAW_BUFFER_POOL_CAPACITY: usize = 256;
pub const DISK_QUEUE_CAPACITY: usize = RAW_BUFFER_POOL_CAPACITY;
pub const PREVIEW_PACKET_POOL_CAPACITY: usize = 4;
pub const PREVIEW_PACKET_QUEUE_CAPACITY: usize = 4;
pub const PREVIEW_FRAME_QUEUE_CAPACITY: usize = 4;
const PREVIEW_FRAME_POOL_CAPACITY: usize = PREVIEW_FRAME_QUEUE_CAPACITY * 2;
const DEFAULT_DISK_WRITER_BUFFER_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_EVENT_RING_CAPACITY_EVENTS: usize = 1_000_000;
pub const N_BUFFERS: usize = RAW_BUFFER_POOL_CAPACITY;
const CURRENT_RATE_WINDOW: Duration = Duration::from_secs(1);
/// Upper bound on trigger edges held as pending preview-frame annotations.
/// Only a CD-driven frame emission or the EOF flush drains them, so a stream
/// without CD events needs this cap to stay bounded. At 16 B per edge this is
/// ~1 MB, far more than any single frame window annotates.
const MAX_PENDING_TRIGGERS: usize = 65_536;
/// How often the control thread re-reads the sensor monitoring block while a
/// consumer asks for it. Each poll is a handful of USB control transfers, and
/// the values it reports (dead time, illumination, die temperature) drift far
/// slower than this, so 500 ms keeps the panel live without adding traffic
/// that matters next to streaming.
const SENSOR_MONITORING_INTERVAL: Duration = Duration::from_millis(500);
const SENSOR_TELEMETRY_QUEUE_CAPACITY: usize = 64;
/// How long the stream loop waits for a free raw buffer before rechecking the
/// stop flag and the control queue.
const POOL_WAIT_WINDOW: Duration = Duration::from_millis(10);
/// Slice the stream loop parks on the buffer pool between transport service
/// calls while the pool is empty.
const POOL_WAIT_SERVICE_SLICE: Duration = Duration::from_millis(1);
/// How long the post-stop drain waits for a raw buffer to move data the reader
/// already received into. Generous on purpose: this runs once per recording and
/// the alternative is losing the recording's tail.
const STOP_DRAIN_POOL_WAIT: Duration = Duration::from_millis(200);
/// Hard bounds on the post-stop drain. The camera is not guaranteed to have
/// stopped streaming yet (the control thread stops it on its own tick), so a
/// reader that keeps re-arming its transfers can keep producing packets. The
/// drain exists to flush what the host already held, not to keep recording, so
/// it must terminate on its own.
const STOP_DRAIN_DEADLINE: Duration = Duration::from_millis(500);
/// Packet cap for the same reason. Comfortably above any reader's in-flight
/// plus buffered transfer count (EVK4: 8 queued + 8 spare).
const STOP_DRAIN_MAX_PACKETS: u64 = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CdEvent {
    pub x: u16,
    pub y: u16,
    pub timestamp: u64,
    pub polarity: bool,
}

impl From<CdEvent> for CompactEvent {
    fn from(event: CdEvent) -> Self {
        Self::new(event.x, event.y, event.timestamp, u8::from(event.polarity))
    }
}

impl From<CompactEvent> for CdEvent {
    fn from(event: CompactEvent) -> Self {
        Self {
            x: event.x,
            y: event.y,
            timestamp: event.timestamp_us(),
            polarity: event.is_on(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveEventSource {
    ring: Arc<Mutex<EventRing>>,
    capacity_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveEventFrameBatch {
    pub events: Vec<CompactEvent>,
    pub event_range: Range<u64>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorDrainError {
    MissingCursor,
    OutOfTimeline,
}

impl std::fmt::Display for CursorDrainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCursor => f.write_str("event cursor is not registered"),
            Self::OutOfTimeline => f.write_str("event cursor points outside the resident timeline"),
        }
    }
}

impl std::error::Error for CursorDrainError {}

impl LiveEventSource {
    pub fn with_capacity(capacity_events: usize) -> Self {
        let capacity_events = capacity_events.max(1);
        Self {
            ring: Arc::new(Mutex::new(EventRing::with_capacity(capacity_events))),
            capacity_events,
        }
    }

    pub fn capacity_events(&self) -> usize {
        self.capacity_events
    }

    fn lock_ring(&self) -> std::sync::MutexGuard<'_, EventRing> {
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.ring, &other.ring)
    }

    pub fn next_event_idx(&self) -> u64 {
        self.lock_ring().next_event_idx()
    }

    pub fn append_cd_frame(
        &self,
        events: &[CdEvent],
        window_start_us: u64,
        window_end_us: u64,
    ) -> std::result::Result<Range<u64>, RingAppendError> {
        let compact_events: Vec<_> = events.iter().copied().map(CompactEvent::from).collect();
        let mut ring = self.lock_ring();
        let handle = ring.append_frame(&compact_events, window_start_us, window_end_us)?;
        debug_assert!(
            ring.resident_byte_count()
                <= self
                    .capacity_events
                    .saturating_mul(std::mem::size_of::<CompactEvent>())
        );
        Ok(handle.first_event_idx..handle.first_event_idx + u64::from(handle.event_count))
    }

    pub fn register_cursor(&self, label: impl Into<String>, policy: CursorPolicy) -> CursorId {
        self.lock_ring().register_cursor(label, policy)
    }

    pub fn unregister_cursor(&self, id: CursorId) -> Option<ConsumerCursor> {
        self.lock_ring().unregister_cursor(id)
    }

    pub fn advance_cursor(&self, id: CursorId, next_event_idx: u64) -> bool {
        self.lock_ring().advance_cursor(id, next_event_idx)
    }

    pub fn cursor_next_event_idx(&self, id: CursorId) -> Option<u64> {
        self.lock_ring()
            .cursor(id)
            .map(ConsumerCursor::next_event_idx)
    }

    pub fn frame_entries_from(&self, next_event_idx: u64) -> Option<Vec<FrameWindowEntry>> {
        self.lock_ring().frame_entries_from(next_event_idx)
    }

    /// All frame windows currently resident in the ring, oldest first.
    pub fn retained_frame_entries(&self) -> Vec<FrameWindowEntry> {
        self.lock_ring().frame_index().entries().cloned().collect()
    }

    pub fn drain_cursor_frames(
        &self,
        cursor: CursorId,
    ) -> std::result::Result<Vec<LiveEventFrameBatch>, CursorDrainError> {
        let mut compact_events = Vec::new();
        let mut batches = Vec::new();
        let mut newest_drained_idx = None;
        let ring = self.lock_ring();
        let next_event_idx = ring
            .cursor(cursor)
            .ok_or(CursorDrainError::MissingCursor)?
            .next_event_idx();
        let entries = ring
            .frame_entries_from(next_event_idx)
            .ok_or(CursorDrainError::OutOfTimeline)?;

        for entry in entries {
            if next_event_idx > entry.first_event_idx {
                return Err(CursorDrainError::OutOfTimeline);
            }
            let range = entry.first_event_idx..entry.end_event_idx();
            if !ring.collect_event_range(range.clone(), &mut compact_events) {
                return Err(CursorDrainError::OutOfTimeline);
            }
            newest_drained_idx = Some(range.end);
            batches.push(LiveEventFrameBatch {
                events: std::mem::take(&mut compact_events),
                event_range: range,
                window_start_us: entry.window_start_us,
                window_end_us: entry.window_end_us,
            });
        }

        if let Some(next) = newest_drained_idx {
            ring.advance_cursor(cursor, next);
        }
        Ok(batches)
    }

    /// Visits every event in `range` under one ring lock without allocating.
    /// Returns false (visiting nothing) when the range is no longer resident.
    pub fn for_each_compact_event_in_range(
        &self,
        range: Range<u64>,
        mut visit: impl FnMut(CompactEvent),
    ) -> bool {
        self.lock_ring().for_each_slice_in_range(range, |slice| {
            for event in slice {
                visit(*event);
            }
        })
    }

    pub fn compact_events_for_range(&self, range: Range<u64>) -> Option<Vec<CompactEvent>> {
        let mut events = Vec::new();
        if !self.lock_ring().collect_event_range(range, &mut events) {
            return None;
        }
        Some(events)
    }

    pub fn events_for_range(&self, range: Range<u64>) -> Option<Vec<CdEvent>> {
        let compact_events = self.compact_events_for_range(range)?;
        Some(compact_events.into_iter().map(CdEvent::from).collect())
    }
}

impl Default for LiveEventSource {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_EVENT_RING_CAPACITY_EVENTS)
    }
}

impl EventSource for LiveEventSource {
    fn fetch_range(
        &self,
        start_us: u64,
        end_us: u64,
    ) -> std::result::Result<EventChunk, FetchError> {
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fetch_range(start_us, end_us)
    }
}

pub trait PreviewDecoder: Send + 'static {
    fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()>;

    /// Appends the external trigger edges decoded by the most recent
    /// `decode_bytes` call to `out`, draining the decoder's internal buffer.
    /// Decoders without a trigger concept keep the default no-op.
    fn take_triggers(&mut self, out: &mut Vec<ExternalTriggerEvent>) {
        let _ = out;
    }

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
    pending_triggers: Vec<ExternalTriggerEvent>,
    timestamp_unwrapper: Evt3TimestampUnwrapper,
    trigger_timestamp_mapper: SecondaryTimestampMapper,
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

        // Triggers arrive in their own vector, so they are unwrapped against
        // the CD stream's rollover epoch instead of being fed through the CD
        // unwrapper (which would clamp mid-packet trigger times forward).
        self.pending_triggers.reserve(self.trigger_scratch.len());
        for trigger in &self.trigger_scratch {
            let timestamp_us = self.trigger_timestamp_mapper.map_timestamp(
                trigger.timestamp,
                self.timestamp_unwrapper.reference_timestamp_us(),
            );
            self.pending_triggers.push(ExternalTriggerEvent::new(
                timestamp_us,
                trigger.id,
                trigger.value != 0,
            ));
        }

        Ok(())
    }

    fn take_triggers(&mut self, out: &mut Vec<ExternalTriggerEvent>) {
        out.append(&mut self.pending_triggers);
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
    pub event_range: Option<Range<u64>>,
    pub event_source: Option<LiveEventSource>,
    /// External trigger edges whose timestamps fall inside this frame's
    /// accumulation window, in timestamp order. Delivery through the bounded
    /// preview channel is best-effort; exact trigger data is recomputed from
    /// the recorded RAW file.
    pub external_triggers: Vec<ExternalTriggerEvent>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

impl PreviewFrame {
    pub fn raw_events_available(&self) -> bool {
        self.events.is_some() || (self.event_source.is_some() && self.event_range.is_some())
    }

    pub fn event_count(&self) -> Option<usize> {
        if let Some(events) = &self.events {
            return Some(events.len());
        }
        let range = self.event_range.as_ref()?;
        usize::try_from(range.end.saturating_sub(range.start)).ok()
    }

    pub fn events_snapshot(&self) -> Option<Vec<CdEvent>> {
        if let Some(events) = &self.events {
            return Some(events.clone());
        }
        let source = self.event_source.as_ref()?;
        source.events_for_range(self.event_range.clone()?)
    }

    pub fn compact_events_snapshot(&self) -> Option<Vec<CompactEvent>> {
        if let Some(events) = &self.events {
            return Some(events.iter().copied().map(CompactEvent::from).collect());
        }
        let source = self.event_source.as_ref()?;
        source.compact_events_for_range(self.event_range.clone()?)
    }
}

pub fn accumulate_compact_frame(
    events: &[CompactEvent],
    external_triggers: &[ExternalTriggerEvent],
    width: u16,
    height: u16,
    window_start_us: u64,
    window_end_us: u64,
    include_raw_events: bool,
) -> PreviewFrame {
    let pixel_count = width as usize * height as usize;
    let mut frame_buffers = take_preview_frame_buffers(pixel_count);
    frame_buffers.pixels.fill(0);
    frame_buffers.pixels_on.fill(0);
    frame_buffers.pixels_off.fill(0);
    reset_histogram_bins(&mut frame_buffers.total_histogram, pixel_count as u64);
    reset_histogram_bins(&mut frame_buffers.signed_histogram, pixel_count as u64);

    let mut on_count = 0_u64;
    let mut off_count = 0_u64;
    let mut raw_events = include_raw_events.then(|| Vec::with_capacity(events.len()));

    for compact in events {
        let event = CdEvent::from(*compact);
        if event.x >= width || event.y >= height {
            continue;
        }
        if let Some(raw_events) = &mut raw_events {
            raw_events.push(event);
        }
        let idx = event.y as usize * width as usize + event.x as usize;
        let old_total = frame_buffers.pixels[idx];
        let new_total = old_total.saturating_add(1);
        frame_buffers.pixels[idx] = new_total;
        transition_histogram_bin(&mut frame_buffers.total_histogram, old_total, new_total);

        let old_on = frame_buffers.pixels_on[idx];
        let old_off = frame_buffers.pixels_off[idx];
        if event.polarity {
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
    }

    PreviewFrame {
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
        event_range: None,
        event_source: None,
        external_triggers: augur_event_types::inclusive_trigger_window(
            external_triggers,
            window_start_us,
            window_end_us,
        )
        .to_vec(),
        window_start_us,
        window_end_us,
    }
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
    pub event_ring_capacity_events: usize,
    pub plugin_event_history: bool,
    pub sensor_telemetry: Option<SensorTelemetryOptions>,
    pub metadata: Option<RecordingMetadata>,
}

/// Optional companion recording for physical sensor monitoring values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorTelemetryOptions {
    pub illumination_interval: Duration,
    pub slow_interval: Duration,
}

impl Default for SensorTelemetryOptions {
    fn default() -> Self {
        Self {
            // 1.5 Hz.
            illumination_interval: Duration::from_nanos(666_666_667),
            // 0.2 Hz.
            slow_interval: Duration::from_secs(5),
        }
    }
}

impl PipelineOptions {
    pub fn new(output_path: impl Into<PathBuf>) -> Self {
        Self {
            output_path: Some(output_path.into()),
            sensor_width: 1280,
            sensor_height: 720,
            write_evt3_header: true,
            disk_writer_buffer_bytes: DEFAULT_DISK_WRITER_BUFFER_BYTES,
            event_ring_capacity_events: DEFAULT_EVENT_RING_CAPACITY_EVENTS,
            plugin_event_history: false,
            sensor_telemetry: None,
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
            event_ring_capacity_events: DEFAULT_EVENT_RING_CAPACITY_EVENTS,
            plugin_event_history: false,
            sensor_telemetry: None,
            metadata: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SensorTelemetrySnapshot {
    pub output_path: Option<PathBuf>,
    pub samples_written: u64,
    pub samples_dropped: u64,
    pub read_errors: u64,
    pub write_error: Option<String>,
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
    /// Total EVT3 `EXT_TRIGGER` edges decoded (both rising and falling),
    /// counted at decode time — before frame windowing, best-effort preview
    /// drops, and the rising-only filter plugins apply. An authoritative
    /// "did any edge reach the host?" signal for TRIG_IN debugging.
    pub triggers_total: u64,
    /// Wall-clock age of the most recently decoded trigger edge, if any.
    pub last_trigger_age_s: Option<f64>,
    /// Trigger edges discarded because the pending-annotation buffer hit
    /// `MAX_PENDING_TRIGGERS`. Nonzero only when CD events are too sparse to
    /// close preview windows (a dark box, or a trigger-only protocol), where
    /// the buffer would otherwise grow for the whole session.
    pub triggers_dropped: u64,
    /// How often the stream reader found the raw-buffer pool empty, i.e. the
    /// recording path could not accept the next packet.
    pub raw_pool_starvation_events: u64,
    /// Total time the stream reader spent waiting for a free raw buffer.
    pub raw_pool_starvation_us: u64,
    /// Longest single wait for a free raw buffer. This is the headline
    /// recording-integrity number: the transport only buffers a bounded amount
    /// of data, so a long stall means the camera FIFO overflowed and those
    /// events are unrecoverable.
    pub raw_pool_starvation_max_us: u64,
    /// Packets recovered from the reader after the stream loop stopped, i.e.
    /// data the device had already handed to the host at stop time.
    pub stop_drain_packets: u64,
}

impl PipelineStatsSnapshot {
    /// Whether the recording path ever failed to accept data. Any nonzero
    /// value means the recording may have a gap.
    pub fn recording_may_have_gaps(&self) -> bool {
        self.raw_pool_starvation_events > 0
    }

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

/// Most recent sensor monitoring readback, with enough context for a UI to say
/// how fresh it is and why a field is missing.
#[derive(Debug, Clone, Default)]
pub struct SensorMonitoringSnapshot {
    pub values: SensorMonitoring,
    /// Wall-clock age of the reading.
    pub age_s: f64,
    /// Error from the most recent poll, if it failed. `values` then still holds
    /// the last successful reading.
    pub error: Option<String>,
}

#[derive(Debug)]
struct SensorMonitoringState {
    values: SensorMonitoring,
    at: Instant,
    error: Option<String>,
}

fn merge_monitoring_values(target: &mut SensorMonitoring, update: SensorMonitoring) {
    if update.pixel_dead_time_us.is_some() {
        target.pixel_dead_time_us = update.pixel_dead_time_us;
    }
    if update.illumination_lux.is_some() {
        target.illumination_lux = update.illumination_lux;
    }
    if update.temperature_c.is_some() {
        target.temperature_c = update.temperature_c;
    }
    if update.biases.is_some() {
        target.biases = update.biases;
    }
}

#[derive(Debug)]
struct SensorTelemetrySchedule {
    options: SensorTelemetryOptions,
    next_illumination: Instant,
    next_slow: Instant,
    include_biases: bool,
    next_sample_id: u64,
}

impl SensorTelemetrySchedule {
    fn new(options: SensorTelemetryOptions, now: Instant) -> Self {
        Self {
            options,
            next_illumination: now,
            next_slow: now,
            include_biases: true,
            next_sample_id: 0,
        }
    }

    fn force_bias_readback(&mut self) {
        self.include_biases = true;
    }

    fn due(&mut self, now: Instant) -> Option<(u64, &'static str, SensorMonitoringSelection)> {
        let illumination_due = now >= self.next_illumination;
        let slow_due = now >= self.next_slow;
        if !illumination_due && !slow_due && !self.include_biases {
            return None;
        }

        let mut selection = SensorMonitoringSelection::NONE;
        let poll_kind = match (illumination_due, slow_due, self.include_biases) {
            (true, true, true) => "illumination+slow+biases",
            (true, true, false) => "illumination+slow",
            (true, false, true) => "illumination+biases",
            (false, true, true) => "slow+biases",
            (true, false, false) => "illumination",
            (false, true, false) => "slow",
            (false, false, true) => "biases",
            (false, false, false) => unreachable!(),
        };
        if illumination_due {
            selection = selection.union(SensorMonitoringSelection::ILLUMINATION);
            self.next_illumination = now + self.options.illumination_interval;
        }
        if slow_due {
            selection = selection.union(SensorMonitoringSelection::SLOW_TELEMETRY);
            self.next_slow = now + self.options.slow_interval;
        }
        if self.include_biases {
            selection.biases = true;
            self.include_biases = false;
        }

        let sample_id = self.next_sample_id;
        self.next_sample_id = self.next_sample_id.saturating_add(1);
        Some((sample_id, poll_kind, selection))
    }
}

#[derive(Debug)]
struct SensorTelemetrySample {
    sample_id: u64,
    poll_kind: &'static str,
    host_elapsed_start_us: u64,
    host_elapsed_end_us: u64,
    raw_data_offset_before_bytes: u64,
    raw_data_offset_after_bytes: u64,
    values: SensorMonitoring,
    status: &'static str,
    error: Option<String>,
}

fn csv_optional_f32(value: Option<f32>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn write_sensor_telemetry_sample(
    writer: &mut impl Write,
    sample: &SensorTelemetrySample,
) -> std::io::Result<()> {
    let values = sample.values;
    let biases = values.biases;
    writeln!(
        writer,
        "1,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        sample.sample_id,
        sample.poll_kind,
        sample.host_elapsed_start_us,
        sample.host_elapsed_end_us,
        sample.raw_data_offset_before_bytes,
        sample.raw_data_offset_after_bytes,
        csv_optional_f32(values.illumination_lux),
        csv_optional_f32(values.temperature_c),
        csv_optional_f32(values.pixel_dead_time_us),
        biases
            .map(|value| value.current.diff_on)
            .map_or_else(String::new, |v| v.to_string()),
        biases
            .map(|value| value.current.diff_off)
            .map_or_else(String::new, |v| v.to_string()),
        biases
            .map(|value| value.current.fo)
            .map_or_else(String::new, |v| v.to_string()),
        biases
            .map(|value| value.current.hpf)
            .map_or_else(String::new, |v| v.to_string()),
        biases
            .map(|value| value.current.refr)
            .map_or_else(String::new, |v| v.to_string()),
        sample.status,
        csv_escape(sample.error.as_deref().unwrap_or_default()),
    )
}

fn prepare_sensor_telemetry_writer(path: &Path) -> Result<BufWriter<File>> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "schema_version,sample_id,poll_kind,host_elapsed_start_us,host_elapsed_end_us,\
raw_data_offset_before_bytes,raw_data_offset_after_bytes,illumination_lux,temperature_c,\
pixel_dead_time_us,bias_diff_on_code,bias_diff_off_code,bias_fo_code,bias_hpf_code,\
bias_refr_code,status,error"
    )?;
    Ok(writer)
}

pub fn sensor_telemetry_path(raw_path: &Path) -> Option<PathBuf> {
    let stem = raw_path.file_stem()?.to_string_lossy();
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}.sensor-monitoring.csv")))
}

impl SensorMonitoringState {
    fn snapshot_at(&self, now: Instant) -> SensorMonitoringSnapshot {
        SensorMonitoringSnapshot {
            values: self.values,
            age_s: now.saturating_duration_since(self.at).as_secs_f64(),
            error: self.error.clone(),
        }
    }
}

/// Re-reads the sensor monitoring block when a consumer is asking for it and
/// the previous reading has aged out. Runs on the camera-control thread, which
/// is the only thread allowed to touch the control endpoint.
fn poll_sensor_monitoring(
    camera: &mut dyn EventCamera,
    needed: &AtomicBool,
    state: &Mutex<Option<SensorMonitoringState>>,
    last_poll: &mut Option<Instant>,
) {
    if !needed.load(Ordering::Relaxed) {
        // Drop the stale reading so a reopened panel never shows values from
        // before it was closed as if they were current.
        if last_poll.take().is_some() {
            if let Ok(mut slot) = state.lock() {
                *slot = None;
            }
        }
        return;
    }

    let now = Instant::now();
    if matches!(*last_poll, Some(at) if now.saturating_duration_since(at) < SENSOR_MONITORING_INTERVAL)
    {
        return;
    }
    *last_poll = Some(now);

    let result = camera.read_monitoring_selected(SensorMonitoringSelection::ALL);
    if let Ok(mut slot) = state.lock() {
        match result {
            // A source that reports nothing has no monitoring block. Publishing
            // an all-`None` snapshot would tell a consumer "live values are
            // available, they are just all missing", so publish nothing.
            Ok(values) if values.is_empty() => *slot = None,
            Ok(values) => {
                *slot = Some(SensorMonitoringState {
                    values,
                    at: Instant::now(),
                    error: None,
                })
            }
            Err(err) => {
                let message = err.to_string();
                match slot.as_mut() {
                    // Keep the last good values visible, but mark them stale by
                    // leaving `at` alone so the age keeps growing.
                    Some(existing) => existing.error = Some(message),
                    None => {
                        *slot = Some(SensorMonitoringState {
                            values: SensorMonitoring::default(),
                            at: Instant::now(),
                            error: Some(message),
                        })
                    }
                }
            }
        }
    }
}

fn poll_sensor_telemetry(
    camera: &mut dyn EventCamera,
    schedule: &mut SensorTelemetrySchedule,
    stats: &Mutex<PipelineStatsInner>,
    monitoring_state: &Mutex<Option<SensorMonitoringState>>,
    sample_tx: &Sender<SensorTelemetrySample>,
    telemetry_state: &Mutex<SensorTelemetrySnapshot>,
) {
    let now = Instant::now();
    let Some((sample_id, poll_kind, selection)) = schedule.due(now) else {
        return;
    };
    let (host_elapsed_start_us, raw_data_offset_before_bytes) = stats
        .lock()
        .map(|stats| stats.recording_anchor(now))
        .unwrap_or_default();

    let result = camera.read_monitoring_selected(selection);
    let completed = Instant::now();
    let (host_elapsed_end_us, raw_data_offset_after_bytes) = stats
        .lock()
        .map(|stats| stats.recording_anchor(completed))
        .unwrap_or_default();

    let (values, status, error) = match result {
        Ok(values) if values.is_empty() => (
            values,
            "unsupported",
            Some("no requested monitoring value available".to_owned()),
        ),
        Ok(values) => {
            if let Ok(mut slot) = monitoring_state.lock() {
                match slot.as_mut() {
                    Some(existing) => {
                        merge_monitoring_values(&mut existing.values, values);
                        existing.at = completed;
                        existing.error = None;
                    }
                    None => {
                        *slot = Some(SensorMonitoringState {
                            values,
                            at: completed,
                            error: None,
                        });
                    }
                }
            }
            (values, "valid", None)
        }
        Err(err) => {
            let message = err.to_string();
            if let Ok(mut state) = telemetry_state.lock() {
                state.read_errors = state.read_errors.saturating_add(1);
            }
            if let Ok(mut slot) = monitoring_state.lock() {
                match slot.as_mut() {
                    Some(existing) => existing.error = Some(message.clone()),
                    None => {
                        *slot = Some(SensorMonitoringState {
                            values: SensorMonitoring::default(),
                            at: completed,
                            error: Some(message.clone()),
                        });
                    }
                }
            }
            (SensorMonitoring::default(), "error", Some(message))
        }
    };

    let sample = SensorTelemetrySample {
        sample_id,
        poll_kind,
        host_elapsed_start_us,
        host_elapsed_end_us,
        raw_data_offset_before_bytes,
        raw_data_offset_after_bytes,
        values,
        status,
        error,
    };
    if sample_tx.try_send(sample).is_err() {
        if let Ok(mut state) = telemetry_state.lock() {
            state.samples_dropped = state.samples_dropped.saturating_add(1);
        }
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
    triggers_total: u64,
    triggers_dropped: u64,
    last_trigger_at: Option<Instant>,
    raw_pool_starvation_events: u64,
    raw_pool_starvation_us: u64,
    raw_pool_starvation_max_us: u64,
    stop_drain_packets: u64,
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
            triggers_total: 0,
            triggers_dropped: 0,
            last_trigger_at: None,
            raw_pool_starvation_events: 0,
            raw_pool_starvation_us: 0,
            raw_pool_starvation_max_us: 0,
            stop_drain_packets: 0,
            recent_samples: VecDeque::new(),
        }
    }

    fn record_triggers(&mut self, now: Instant, count: usize) {
        if count > 0 {
            self.triggers_total += count as u64;
            self.last_trigger_at = Some(now);
        }
    }

    fn record_dropped_triggers(&mut self, count: usize) {
        self.triggers_dropped += count as u64;
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

    fn recording_anchor(&self, now: Instant) -> (u64, u64) {
        (
            now.saturating_duration_since(self.started).as_micros() as u64,
            self.bytes_total,
        )
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
            triggers_total: self.triggers_total,
            last_trigger_age_s: self
                .last_trigger_at
                .map(|at| now.saturating_duration_since(at).as_secs_f64()),
            triggers_dropped: self.triggers_dropped,
            raw_pool_starvation_events: self.raw_pool_starvation_events,
            raw_pool_starvation_us: self.raw_pool_starvation_us,
            raw_pool_starvation_max_us: self.raw_pool_starvation_max_us,
            stop_drain_packets: self.stop_drain_packets,
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

    /// Records that the stream reader had to wait for a free raw buffer, i.e.
    /// the recording path could not accept data for `wait`.
    fn record_raw_pool_starvation(&mut self, wait: Duration) {
        let wait_us = wait.as_micros() as u64;
        self.raw_pool_starvation_events = self.raw_pool_starvation_events.saturating_add(1);
        self.raw_pool_starvation_us = self.raw_pool_starvation_us.saturating_add(wait_us);
        self.raw_pool_starvation_max_us = self.raw_pool_starvation_max_us.max(wait_us);
    }

    fn record_stop_drain_packet(&mut self) {
        self.stop_drain_packets = self.stop_drain_packets.saturating_add(1);
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

    #[cfg(test)]
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

/// Caps the pending-annotation buffer at `MAX_PENDING_TRIGGERS`, dropping the
/// oldest edges first, and returns how many were dropped.
///
/// Only a CD-driven frame emission or the EOF flush drains this buffer, so a
/// stream whose CD events are too sparse to close a window (a dark box, or a
/// trigger-only protocol) would otherwise grow it for the whole session. The
/// recent tail is kept because that is what an upcoming frame would annotate.
fn bound_pending_triggers(pending_triggers: &mut Vec<ExternalTriggerEvent>) -> usize {
    let excess = pending_triggers.len().saturating_sub(MAX_PENDING_TRIGGERS);
    if excess > 0 {
        pending_triggers.drain(..excess);
    }
    excess
}

/// Splits off the pending triggers that belong to a frame ending at
/// `window_end_us`. Pending triggers are timestamp-ordered, so this is the
/// prefix with `timestamp_us <= window_end_us`.
fn split_frame_triggers(
    pending_triggers: &mut Vec<ExternalTriggerEvent>,
    window_end_us: u64,
) -> Vec<ExternalTriggerEvent> {
    let split = pending_triggers.partition_point(|trigger| trigger.timestamp_us <= window_end_us);
    let remainder = pending_triggers.split_off(split);
    std::mem::replace(pending_triggers, remainder)
}

#[allow(clippy::too_many_arguments)]
fn emit_preview_frame(
    frame_tx: &Sender<PreviewFrame>,
    stats_preview: &Arc<Mutex<PipelineStatsInner>>,
    event_source: &LiveEventSource,
    recording_cursor: Option<CursorId>,
    frame_buffers: &mut PreviewFrameBuffers,
    frame_events: &mut Vec<CdEvent>,
    pending_triggers: &mut Vec<ExternalTriggerEvent>,
    capture_raw_events: bool,
    on_count: &mut u64,
    off_count: &mut u64,
    frame_start_ts: &mut Option<u64>,
    width: u16,
    height: u16,
    pixel_count: usize,
    window_end_us: u64,
) {
    let Some(window_start_us) = *frame_start_ts else {
        return;
    };

    // Triggers assigned to this frame leave the pending queue either way:
    // if the frame is dropped below, its triggers drop with it (live preview
    // delivery is best-effort; replayed RAW data is exact).
    let frame_triggers = split_frame_triggers(pending_triggers, window_end_us);

    let event_range = if !capture_raw_events || frame_events.is_empty() {
        None
    } else {
        // Archiving a frame's raw events into the shared ring is best-effort.
        // A single accumulation window can legitimately exceed the ring
        // capacity (high event rates, or a long replay window), in which case
        // `append_cd_frame` returns `FrameTooLarge`. That must NOT tear down the
        // preview thread: the visual frame is already fully accumulated and can
        // still be shown. We emit it without a ring-backed event range and count
        // the archival miss as a frame drop for visibility.
        match event_source.append_cd_frame(frame_events, window_start_us, window_end_us) {
            Ok(range) => {
                if let Some(cursor) = recording_cursor {
                    event_source.advance_cursor(cursor, range.end);
                }
                Some(range)
            }
            Err(_err) => {
                if let Ok(mut s) = stats_preview.lock() {
                    s.record_preview_frame_drop();
                }
                None
            }
        }
    };

    if frame_tx.is_full() {
        reset_preview_frame_accumulators(
            frame_buffers,
            frame_events,
            on_count,
            off_count,
            frame_start_ts,
        );
        if let Ok(mut s) = stats_preview.lock() {
            s.record_preview_frame_drop();
        }
        return;
    }

    let frame_send_started = Instant::now();
    let raw_events = if capture_raw_events {
        let next_capacity = frame_events.capacity().max(8_192);
        Some(std::mem::replace(
            frame_events,
            Vec::with_capacity(next_capacity),
        ))
    } else {
        frame_events.clear();
        None
    };
    let frame_buffers = std::mem::replace(frame_buffers, take_preview_frame_buffers(pixel_count));
    let frame = PreviewFrame {
        width,
        height,
        pixels: frame_buffers.pixels,
        pixels_on: frame_buffers.pixels_on,
        pixels_off: frame_buffers.pixels_off,
        cached_total_histogram: frame_buffers.total_histogram,
        cached_signed_histogram: frame_buffers.signed_histogram,
        on_count: *on_count,
        off_count: *off_count,
        events: raw_events,
        event_range,
        event_source: Some(event_source.clone()),
        external_triggers: frame_triggers,
        window_start_us,
        window_end_us,
    };
    match frame_tx.try_send(frame) {
        Ok(()) => {
            if let Ok(mut s) = stats_preview.lock() {
                s.record_preview_frame_queue_depth(frame_tx.len());
                s.record_preview_frame_send_time(frame_send_started.elapsed());
            }
        }
        Err(_err) => {
            if let Ok(mut s) = stats_preview.lock() {
                s.record_preview_frame_drop();
            }
        }
    }
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

/// The stream thread's packet source. Inline mode owns the camera and pumps
/// runtime reconfiguration between reads; split mode reads from a detached
/// stream reader while a dedicated control thread owns the camera, so
/// reconfiguration never pauses stream reads.
trait StreamWorker: Send {
    fn start(&mut self) -> std::result::Result<(), String>;
    fn poll_control(&mut self, error_tx: &Sender<String>);
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize>;
    /// Keeps the transport serviced while the loop has no buffer to read into.
    /// See [`PacketStreamReader::service`].
    fn service(&mut self, budget: Duration);
    /// Drains data the reader already received once the loop has stopped.
    /// See [`PacketStreamReader::take_buffered_packet`].
    fn take_buffered_packet(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn finish(&mut self) -> std::result::Result<(), String>;
}

struct InlineCameraWorker<C: PacketStreamCamera> {
    camera: C,
    initial_config: CameraConfig,
    settings_rx: Receiver<CameraConfig>,
}

impl<C: PacketStreamCamera> StreamWorker for InlineCameraWorker<C> {
    fn start(&mut self) -> std::result::Result<(), String> {
        self.camera
            .configure(&self.initial_config)
            .map_err(|e| format!("initial camera configure failed: {e}"))?;
        self.camera
            .start_streaming()
            .map_err(|e| format!("camera start_streaming failed: {e}"))
    }

    fn poll_control(&mut self, error_tx: &Sender<String>) {
        while let Ok(cfg) = self.settings_rx.try_recv() {
            if let Err(e) = self.camera.configure(&cfg) {
                let _ = error_tx.try_send(format!("usb: runtime reconfigure failed: {e}"));
            }
        }
    }

    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.camera.read_packet(buf)
    }

    // Inline cameras read synchronously: nothing is queued in the transport
    // that could go stale while the pipeline waits for a buffer.
    fn service(&mut self, _budget: Duration) {}

    fn take_buffered_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
        Ok(0)
    }

    fn finish(&mut self) -> std::result::Result<(), String> {
        self.camera
            .stop_streaming()
            .map_err(|e| format!("camera stop_streaming failed: {e}"))
    }
}

struct SplitStreamWorker {
    reader: Box<dyn PacketStreamReader>,
}

impl StreamWorker for SplitStreamWorker {
    fn start(&mut self) -> std::result::Result<(), String> {
        Ok(())
    }

    fn poll_control(&mut self, _error_tx: &Sender<String>) {}

    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.reader.read_packet(buf)
    }

    fn service(&mut self, budget: Duration) {
        self.reader.service(budget);
    }

    fn take_buffered_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.reader.take_buffered_packet(buf)
    }

    fn finish(&mut self) -> std::result::Result<(), String> {
        Ok(())
    }
}

enum RawBufferWait {
    Buffer(UsbBuffer),
    Timeout,
    Disconnected,
}

/// Takes a free raw buffer, keeping the transport serviced while none is
/// available.
///
/// The buffer pool is the pipeline's backpressure: while it is empty the
/// recording path cannot accept data. Blocking here without servicing the
/// transport would let its queued transfers go stale, so the device endpoint
/// runs dry and the camera FIFO overflows — turning a short downstream hiccup
/// into a much longer, unrecoverable gap. Every wait is recorded, because a
/// recording that hit this path may be incomplete.
fn acquire_raw_buffer(
    worker: &mut dyn StreamWorker,
    stop: &AtomicBool,
    stats: &Mutex<PipelineStatsInner>,
    pool_rx: &Receiver<UsbBuffer>,
) -> RawBufferWait {
    match pool_rx.try_recv() {
        Ok(buf) => return RawBufferWait::Buffer(buf),
        Err(TryRecvError::Disconnected) => return RawBufferWait::Disconnected,
        Err(TryRecvError::Empty) => {}
    }

    let started = Instant::now();
    let outcome = loop {
        // Zero budget: reap and re-arm whatever the transport already
        // completed without blocking, then park on the pool for a slice.
        worker.service(Duration::ZERO);
        match pool_rx.recv_timeout(POOL_WAIT_SERVICE_SLICE) {
            Ok(buf) => break RawBufferWait::Buffer(buf),
            Err(RecvTimeoutError::Disconnected) => break RawBufferWait::Disconnected,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::Relaxed) || started.elapsed() >= POOL_WAIT_WINDOW {
            break RawBufferWait::Timeout;
        }
    };
    if let Ok(mut s) = stats.lock() {
        s.record_raw_pool_starvation(started.elapsed());
    }
    outcome
}

/// Moves packets the reader already received into the recording after the
/// stream loop stopped.
///
/// The transport keeps several transfers in flight, so at stop time the host
/// usually already holds data the device sent. Tearing the reader down without
/// draining it truncates the tail of every recording.
fn drain_reader_after_stop(
    worker: &mut dyn StreamWorker,
    stats: &Mutex<PipelineStatsInner>,
    pool_rx: &Receiver<UsbBuffer>,
    pool_return_tx: &Sender<UsbBuffer>,
    disk_tx: &Sender<DiskChunk>,
) {
    let deadline = Instant::now() + STOP_DRAIN_DEADLINE;
    let mut drained = 0_u64;
    while drained < STOP_DRAIN_MAX_PACKETS && Instant::now() < deadline {
        let Ok(mut buf) = pool_rx.recv_timeout(STOP_DRAIN_POOL_WAIT) else {
            return;
        };
        let taken = worker.take_buffered_packet(&mut buf[..]);
        match taken {
            Ok(len) if len > 0 => {
                if disk_tx.send(DiskChunk { buf, len }).is_err() {
                    return;
                }
                drained += 1;
                if let Ok(mut s) = stats.lock() {
                    s.record_stop_drain_packet();
                }
            }
            _ => {
                let _ = pool_return_tx.send(buf);
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_stream_loop(
    worker: &mut dyn StreamWorker,
    stop: &AtomicBool,
    stats: &Mutex<PipelineStatsInner>,
    error_tx: &Sender<String>,
    pool_rx: &Receiver<UsbBuffer>,
    pool_return_tx: &Sender<UsbBuffer>,
    preview_pool_rx: &Receiver<UsbBuffer>,
    preview_pool_return_tx: &Sender<UsbBuffer>,
    preview_tx: &Sender<PreviewChunk>,
    disk_tx: Option<&Sender<DiskChunk>>,
) {
    while !stop.load(Ordering::Relaxed) {
        worker.poll_control(error_tx);

        let mut buf = match acquire_raw_buffer(worker, stop, stats, pool_rx) {
            RawBufferWait::Buffer(buf) => buf,
            RawBufferWait::Timeout => continue,
            RawBufferWait::Disconnected => break,
        };

        let len = match worker.read_packet(&mut buf[..]) {
            Ok(n) => n,
            Err(CameraError::Timeout(_)) => {
                let _ = pool_return_tx.send(buf);
                continue;
            }
            Err(CameraError::Eof) => {
                let _ = pool_return_tx.send(buf);
                stop.store(true, Ordering::Relaxed);
                break;
            }
            Err(e) => {
                report_pipeline_error(error_tx, stop, "usb", format!("stream read failed: {e}"));
                let _ = pool_return_tx.send(buf);
                break;
            }
        };

        if len == 0 {
            let _ = pool_return_tx.send(buf);
            continue;
        }

        if let Ok(mut s) = stats.lock() {
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
                            if let Ok(mut s) = stats.lock() {
                                s.record_preview_packet_queue_depth(preview_tx.len());
                            }
                        }
                        Err(TrySendError::Full(chunk)) | Err(TrySendError::Disconnected(chunk)) => {
                            if let Ok(mut s) = stats.lock() {
                                s.record_preview_packet_drop();
                            }
                            let _ = preview_pool_return_tx.try_send(chunk.buf);
                        }
                    }
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {
                    if let Ok(mut s) = stats.lock() {
                        s.record_preview_packet_drop();
                    }
                }
            }
        } else if let Ok(mut s) = stats.lock() {
            s.record_preview_packet_drop();
        }

        if let Some(disk_tx) = disk_tx {
            let disk_send_started = Instant::now();
            match disk_tx.send_timeout(DiskChunk { buf, len }, Duration::from_millis(50)) {
                Ok(_) => {
                    if let Ok(mut s) = stats.lock() {
                        s.record_disk_send_wait(disk_send_started.elapsed());
                        s.record_disk_queue_depth(disk_tx.len());
                    }
                }
                Err(SendTimeoutError::Timeout(chunk)) => match disk_tx.send(chunk) {
                    Ok(_) => {
                        if let Ok(mut s) = stats.lock() {
                            s.record_disk_send_wait(disk_send_started.elapsed());
                            s.record_disk_queue_depth(disk_tx.len());
                        }
                    }
                    Err(e) => {
                        if let Ok(mut s) = stats.lock() {
                            s.record_disk_send_wait(disk_send_started.elapsed());
                        }
                        report_pipeline_error(
                            error_tx,
                            stop,
                            "usb",
                            format!("disk channel send failed: {e}"),
                        );
                        break;
                    }
                },
                Err(SendTimeoutError::Disconnected(chunk)) => {
                    if let Ok(mut s) = stats.lock() {
                        s.record_disk_send_wait(disk_send_started.elapsed());
                    }
                    let _ = pool_return_tx.send(chunk.buf);
                    report_pipeline_error(
                        error_tx,
                        stop,
                        "usb",
                        "disk channel disconnected".to_string(),
                    );
                    break;
                }
            }
        } else {
            let _ = pool_return_tx.send(buf);
        }
    }

    if let Some(disk_tx) = disk_tx {
        drain_reader_after_stop(worker, stats, pool_rx, pool_return_tx, disk_tx);
    }
}

pub struct PipelineController {
    pub frame_rx: Receiver<PreviewFrame>,
    pub settings_tx: Sender<CameraConfig>,
    pub acq_time_us: Arc<AtomicU64>,
    pub raw_events_needed: Arc<AtomicBool>,
    /// Demand flag for the sensor monitoring readback. The control thread only
    /// spends USB control transfers on it while this is set, so a consumer
    /// (e.g. an open settings panel) has to ask.
    pub sensor_monitoring_needed: Arc<AtomicBool>,
    pub event_source: LiveEventSource,
    pub plugin_event_cursor: Option<CursorId>,
    error_rx: Receiver<String>,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<PipelineStatsInner>>,
    sensor_monitoring: Arc<Mutex<Option<SensorMonitoringState>>>,
    sensor_telemetry: Arc<Mutex<SensorTelemetrySnapshot>>,
    recording_sidecar: Option<RecordingSidecarState>,
    threads: Vec<thread::JoinHandle<()>>,
    sensor_telemetry_thread: Option<thread::JoinHandle<()>>,
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

    /// Latest sensor monitoring readback, or `None` until the first successful
    /// poll after [`Self::sensor_monitoring_needed`] was set. Always `None` for
    /// sources whose camera runs inline on the stream thread — polling there
    /// would pause packet reads and cost recorded events.
    pub fn sensor_monitoring(&self) -> Option<SensorMonitoringSnapshot> {
        let now = Instant::now();
        self.sensor_monitoring
            .lock()
            .ok()?
            .as_ref()
            .map(|state| state.snapshot_at(now))
    }

    pub fn sensor_telemetry(&self) -> SensorTelemetrySnapshot {
        self.sensor_telemetry
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default()
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
        if let Some(handle) = self.sensor_telemetry_thread.take() {
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
        let sensor_telemetry_snapshot = self.sensor_telemetry();
        if let Some(recording_sidecar) = &mut self.recording_sidecar {
            recording_sidecar.metadata.update_timing(
                stats_snapshot.recording_duration_us,
                stats_snapshot.events_total,
            );
            recording_sidecar.metadata.update_integrity(
                stats_snapshot.raw_pool_starvation_events,
                stats_snapshot.raw_pool_starvation_max_us,
                stats_snapshot.raw_pool_starvation_us,
            );
            if sensor_telemetry_snapshot.output_path.is_some() {
                recording_sidecar.metadata.extra.insert(
                    "sensor_monitoring_samples_written".into(),
                    sensor_telemetry_snapshot.samples_written.to_string(),
                );
                recording_sidecar.metadata.extra.insert(
                    "sensor_monitoring_samples_dropped".into(),
                    sensor_telemetry_snapshot.samples_dropped.to_string(),
                );
                recording_sidecar.metadata.extra.insert(
                    "sensor_monitoring_read_errors".into(),
                    sensor_telemetry_snapshot.read_errors.to_string(),
                );
                if let Some(error) = sensor_telemetry_snapshot.write_error {
                    recording_sidecar
                        .metadata
                        .extra
                        .insert("sensor_monitoring_write_error".into(), error);
                }
            }
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
        event_ring_capacity_events,
        plugin_event_history,
        sensor_telemetry,
        metadata,
    } = options;
    let recording = output_path.is_some();
    let mut recording_sidecar = None;
    if sensor_telemetry.is_some() && output_path.is_none() {
        return Err(CameraError::Config(
            "sensor telemetry requires a raw recording output path".into(),
        ));
    }
    if sensor_telemetry.is_some_and(|options| {
        options.illumination_interval.is_zero() || options.slow_interval.is_zero()
    }) {
        return Err(CameraError::Config(
            "sensor telemetry intervals must be greater than zero".into(),
        ));
    }
    let sensor_telemetry_path = sensor_telemetry
        .and(output_path.as_deref())
        .and_then(sensor_telemetry_path);
    let mut recording_metadata = metadata.unwrap_or_default();
    if let (Some(options), Some(path)) = (sensor_telemetry, sensor_telemetry_path.as_ref()) {
        recording_metadata.extra.insert(
            "sensor_monitoring_file".into(),
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        );
        recording_metadata.extra.insert(
            "sensor_monitoring_clock".into(),
            "host_monotonic_with_raw_data_offset_bracket".into(),
        );
        recording_metadata.extra.insert(
            "sensor_monitoring_illumination_hz".into(),
            format!("{:.6}", 1.0 / options.illumination_interval.as_secs_f64()),
        );
        recording_metadata.extra.insert(
            "sensor_monitoring_slow_hz".into(),
            format!("{:.6}", 1.0 / options.slow_interval.as_secs_f64()),
        );
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
    let sensor_telemetry_writer = match sensor_telemetry_path.as_deref() {
        Some(path) => match prepare_sensor_telemetry_writer(path) {
            Ok(writer) => Some(writer),
            Err(err) => {
                drop(disk_writer);
                if let Some(raw_path) = output_path.as_deref() {
                    let _ = std::fs::remove_file(raw_path);
                }
                return Err(err);
            }
        },
        None => None,
    };

    if let Some(ref output_path) = output_path {
        if let Some(config_path) = recording_config_path(output_path) {
            if let Err(err) =
                RecordingSidecar::new(initial_config.clone(), recording_metadata.clone())
                    .save_to_path(&config_path)
            {
                drop(disk_writer);
                drop(sensor_telemetry_writer);
                let _ = std::fs::remove_file(output_path);
                if let Some(path) = sensor_telemetry_path.as_deref() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(err);
            }
            recording_sidecar = Some(RecordingSidecarState {
                path: config_path,
                config: initial_config.clone(),
                metadata: recording_metadata.clone(),
            });
        }
    }

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
    let (sensor_telemetry_tx, sensor_telemetry_rx) = sensor_telemetry_writer
        .as_ref()
        .map(|_| bounded::<SensorTelemetrySample>(SENSOR_TELEMETRY_QUEUE_CAPACITY))
        .unzip();

    let acq_time_us = Arc::new(AtomicU64::new(
        initial_config
            .global
            .acq_time_ms
            .max(1)
            .saturating_mul(1_000),
    ));
    let raw_events_needed = Arc::new(AtomicBool::new(false));
    let sensor_monitoring_needed = Arc::new(AtomicBool::new(false));
    let sensor_monitoring = Arc::new(Mutex::new(None::<SensorMonitoringState>));
    let sensor_telemetry_state = Arc::new(Mutex::new(SensorTelemetrySnapshot {
        output_path: sensor_telemetry_path.clone(),
        ..SensorTelemetrySnapshot::default()
    }));
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Mutex::new(PipelineStatsInner::new()));
    let event_source = LiveEventSource::with_capacity(event_ring_capacity_events);
    let recording_event_cursor = recording.then(|| {
        event_source.register_cursor(
            "recorder",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::BlockWriter { max_block_us: 0 },
            },
        )
    });
    let plugin_event_cursor = plugin_event_history.then(|| {
        event_source.register_cursor(
            "plugin-runtime",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        )
    });

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

    let mut threads = Vec::new();
    let mut camera = camera;
    let stream_reader = camera.split_stream_reader();
    if sensor_telemetry.is_some() && stream_reader.is_none() {
        drop(disk_writer);
        drop(sensor_telemetry_writer);
        if let Some(path) = output_path.as_deref() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(path) = sensor_telemetry_path.as_deref() {
            let _ = std::fs::remove_file(path);
        }
        if let Some(sidecar) = recording_sidecar.as_ref() {
            let _ = std::fs::remove_file(&sidecar.path);
        }
        return Err(CameraError::Config(
            "sensor telemetry requires a camera with an independent control thread".into(),
        ));
    }
    let sensor_telemetry_thread =
        sensor_telemetry_writer
            .zip(sensor_telemetry_rx)
            .map(|(mut writer, sample_rx)| {
                let telemetry_state = Arc::clone(&sensor_telemetry_state);
                thread::spawn(move || {
                    while let Ok(sample) = sample_rx.recv() {
                        if let Err(err) = write_sensor_telemetry_sample(&mut writer, &sample) {
                            if let Ok(mut state) = telemetry_state.lock() {
                                state.write_error =
                                    Some(format!("failed writing sensor telemetry: {err}"));
                            }
                            return;
                        }
                        if let Ok(mut state) = telemetry_state.lock() {
                            state.samples_written = state.samples_written.saturating_add(1);
                        }
                    }
                    if let Err(err) = writer.flush() {
                        if let Ok(mut state) = telemetry_state.lock() {
                            state.write_error =
                                Some(format!("failed flushing sensor telemetry: {err}"));
                        }
                    }
                })
            });

    let mut worker: Box<dyn StreamWorker> = match stream_reader {
        Some(reader) => {
            // Camera control (initial configure, start/stop streaming, and
            // runtime reconfiguration) runs on a dedicated thread: control
            // transfers can take tens of milliseconds, and a paused stream
            // reader overflows the camera FIFO and leaves gaps in the
            // recording.
            let stop_control = Arc::clone(&stop);
            let error_control = error_tx.clone();
            let monitoring_needed = Arc::clone(&sensor_monitoring_needed);
            let monitoring_state = Arc::clone(&sensor_monitoring);
            let telemetry_stats = Arc::clone(&stats);
            let telemetry_state = Arc::clone(&sensor_telemetry_state);
            let telemetry_sample_tx = sensor_telemetry_tx;
            let mut telemetry_schedule = sensor_telemetry
                .map(|options| SensorTelemetrySchedule::new(options, Instant::now()));
            let control_thread = thread::spawn(move || {
                if let Err(e) = camera.configure(&initial_config) {
                    report_pipeline_error(
                        &error_control,
                        &stop_control,
                        "control",
                        format!("initial camera configure failed: {e}"),
                    );
                    return;
                }
                if let Err(e) = camera.start_streaming() {
                    report_pipeline_error(
                        &error_control,
                        &stop_control,
                        "control",
                        format!("camera start_streaming failed: {e}"),
                    );
                    return;
                }
                let mut last_monitoring_poll = None;
                loop {
                    match settings_rx.recv_timeout(Duration::from_millis(50)) {
                        Ok(cfg) => {
                            last_monitoring_poll = None;
                            match camera.configure(&cfg) {
                                Ok(()) => {
                                    // New bias codes are live now; include them
                                    // in the next telemetry poll.
                                    if let Some(schedule) = telemetry_schedule.as_mut() {
                                        schedule.force_bias_readback();
                                    }
                                }
                                Err(e) => {
                                    let _ = error_control.try_send(format!(
                                        "control: runtime reconfigure failed: {e}"
                                    ));
                                }
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            if stop_control.load(Ordering::Relaxed) {
                                break;
                            }
                            if let (Some(schedule), Some(sample_tx)) =
                                (telemetry_schedule.as_mut(), telemetry_sample_tx.as_ref())
                            {
                                poll_sensor_telemetry(
                                    &mut camera,
                                    schedule,
                                    &telemetry_stats,
                                    &monitoring_state,
                                    sample_tx,
                                    &telemetry_state,
                                );
                            } else {
                                poll_sensor_monitoring(
                                    &mut camera,
                                    &monitoring_needed,
                                    &monitoring_state,
                                    &mut last_monitoring_poll,
                                );
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                if let Err(e) = camera.stop_streaming() {
                    report_pipeline_error(
                        &error_control,
                        &stop_control,
                        "control",
                        format!("camera stop_streaming failed: {e}"),
                    );
                }
            });
            threads.push(control_thread);
            Box::new(SplitStreamWorker { reader })
        }
        None => Box::new(InlineCameraWorker {
            camera,
            initial_config,
            settings_rx,
        }),
    };

    let usb_thread = thread::spawn(move || {
        if let Err(message) = worker.start() {
            report_pipeline_error(&error_usb, &stop_usb, "usb", message);
            return;
        }

        run_stream_loop(
            worker.as_mut(),
            &stop_usb,
            &stats_usb,
            &error_usb,
            &pool_rx,
            &usb_pool_tx,
            &preview_pool_rx,
            &preview_pool_tx_usb,
            &preview_tx,
            disk_tx.as_ref(),
        );

        if let Err(message) = worker.finish() {
            report_pipeline_error(&error_usb, &stop_usb, "usb", message);
        }
    });
    threads.push(usb_thread);

    if let (Some(disk_rx), Some(mut writer)) = (disk_rx, disk_writer) {
        let stop_disk = Arc::clone(&stop);
        let error_disk = error_tx.clone();
        let disk_pool_tx = pool_tx.clone();
        let stats_disk = Arc::clone(&stats);

        let disk_thread = thread::spawn(move || {
            // Recording is lossless: drain until the USB thread drops its
            // sender. Exiting on the stop flag instead would race the USB
            // thread's final send (it may still be blocked in read_packet
            // when stop is set) and silently drop the tail of the recording.
            while let Ok(chunk) = disk_rx.recv() {
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
    let event_source_preview = event_source.clone();
    let preview_thread = thread::spawn(move || {
        let mut events = Vec::<CdEvent>::with_capacity(4_096);
        let mut frame_events = Vec::<CdEvent>::with_capacity(8_192);
        let mut pending_triggers = Vec::<ExternalTriggerEvent>::with_capacity(64);
        let mut frame_buffers = take_preview_frame_buffers(pixel_count);
        let mut on_count = 0_u64;
        let mut off_count = 0_u64;
        let mut frame_start_ts: Option<u64> = None;
        let mut last_event_ts: Option<u64> = None;

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
                    let pending_trigger_watermark = pending_triggers.len();
                    decoder.take_triggers(&mut pending_triggers);
                    let packet_triggers = &pending_triggers[pending_trigger_watermark..];
                    if let Ok(mut s) = stats_preview.lock() {
                        let now = Instant::now();
                        s.record_packet(now, 0, events.len() as u64);
                        s.record_triggers(now, packet_triggers.len());
                        let first = events
                            .first()
                            .map(|event| event.timestamp)
                            .into_iter()
                            .chain(packet_triggers.first().map(|t| t.timestamp_us))
                            .min();
                        let last = events
                            .last()
                            .map(|event| event.timestamp)
                            .into_iter()
                            .chain(packet_triggers.last().map(|t| t.timestamp_us))
                            .max();
                        if let (Some(first), Some(last)) = (first, last) {
                            s.record_event_timestamps(first, last);
                        }
                    }
                    // Triggers are annotations on the live preview: CD events
                    // alone open and close frame windows, and edges ride along
                    // on whichever frame their timestamp falls in (attached in
                    // `emit_preview_frame` via `split_frame_triggers`). We
                    // deliberately do NOT open or force-close windows on trigger
                    // time here — doing so injected empty/partial black frames
                    // whenever an edge ran ahead of a sparse CD stream, which
                    // showed up as preview flicker. Exact trigger-only windows
                    // for A2-style latency protocols come from the replay /
                    // offline `fetch_range` path, not this live preview.
                    //
                    // Because only a CD-driven frame or the EOF flush drains
                    // this buffer, a stream with no CD events (dark box) would
                    // otherwise accumulate edges for the whole session. Cap it
                    // and drop oldest-first: the recent tail is what a frame
                    // would annotate anyway, and the drop is reported in stats
                    // rather than being silent.
                    let dropped = bound_pending_triggers(&mut pending_triggers);
                    if dropped > 0 {
                        if let Ok(mut s) = stats_preview.lock() {
                            s.record_dropped_triggers(dropped);
                        }
                    }
                    let accumulate_started = Instant::now();
                    let capture_raw_events = raw_events_preview.load(Ordering::Relaxed);
                    for ev in &events {
                        if ev.x >= width || ev.y >= height {
                            continue;
                        }
                        if capture_raw_events {
                            frame_events.push(*ev);
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
                        last_event_ts = Some(ev.timestamp);

                        if frame_start_ts.is_some_and(|t0| {
                            ev.timestamp.saturating_sub(t0) >= acq_preview.load(Ordering::Relaxed)
                        }) {
                            emit_preview_frame(
                                &frame_tx,
                                &stats_preview,
                                &event_source_preview,
                                recording_event_cursor,
                                &mut frame_buffers,
                                &mut frame_events,
                                &mut pending_triggers,
                                capture_raw_events,
                                &mut on_count,
                                &mut off_count,
                                &mut frame_start_ts,
                                width,
                                height,
                                pixel_count,
                                ev.timestamp,
                            );
                        }
                    }
                    // (Intentionally no trigger-driven window close here — see
                    // the annotations-only note above. Trailing edges past the
                    // last CD frame are carried on the next CD frame, or flushed
                    // at EOF below so they are never silently dropped.)
                    let accumulate_elapsed = accumulate_started.elapsed();
                    if let Ok(mut s) = stats_preview.lock() {
                        s.record_preview_accumulate_time(accumulate_elapsed);
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

        // End of stream: flush the final partial window when it carries
        // pending trigger edges, so triggers are never silently discarded
        // at EOF (trigger-only recordings depend on this to be visible at
        // all). CD-only partial windows keep the historical behavior of not
        // emitting a below-acq-width tail frame.
        decoder.take_triggers(&mut pending_triggers);
        if frame_start_ts.is_none() {
            frame_start_ts = pending_triggers.first().map(|t| t.timestamp_us);
        }
        if let (Some(t0), false) = (frame_start_ts, pending_triggers.is_empty()) {
            let window_end_us = pending_triggers
                .last()
                .map(|t| t.timestamp_us)
                .into_iter()
                .chain(last_event_ts)
                .max()
                .unwrap_or(t0)
                .max(t0);
            emit_preview_frame(
                &frame_tx,
                &stats_preview,
                &event_source_preview,
                recording_event_cursor,
                &mut frame_buffers,
                &mut frame_events,
                &mut pending_triggers,
                raw_events_preview.load(Ordering::Relaxed),
                &mut on_count,
                &mut off_count,
                &mut frame_start_ts,
                width,
                height,
                pixel_count,
                window_end_us,
            );
        }
    });

    threads.push(preview_thread);

    Ok(PipelineController {
        frame_rx,
        settings_tx,
        acq_time_us,
        raw_events_needed,
        sensor_monitoring_needed,
        event_source,
        plugin_event_cursor,
        error_rx,
        stop,
        stats,
        sensor_monitoring,
        sensor_telemetry: sensor_telemetry_state,
        recording_sidecar,
        threads,
        sensor_telemetry_thread,
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
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_path)?;
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
    use crate::camera::{DeviceInfo, EventCamera};
    use crate::metadata::RecordingMetadata;

    #[test]
    fn sensor_telemetry_companion_path_keeps_the_raw_stem() {
        assert_eq!(
            sensor_telemetry_path(Path::new("/tmp/experiment.v2.raw")),
            Some(PathBuf::from("/tmp/experiment.v2.sensor-monitoring.csv"))
        );
    }

    #[test]
    fn sensor_telemetry_schedule_uses_field_specific_rates_without_catch_up() {
        let started = Instant::now();
        let mut schedule = SensorTelemetrySchedule::new(
            SensorTelemetryOptions {
                illumination_interval: Duration::from_millis(667),
                slow_interval: Duration::from_secs(5),
            },
            started,
        );

        let (_, _, initial) = schedule.due(started).expect("initial sample");
        assert!(initial.illumination);
        assert!(initial.temperature);
        assert!(initial.pixel_dead_time);
        assert!(initial.biases);
        assert!(schedule.due(started + Duration::from_millis(666)).is_none());

        let (_, kind, fast) = schedule
            .due(started + Duration::from_millis(667))
            .expect("illumination sample");
        assert_eq!(kind, "illumination");
        assert_eq!(fast, SensorMonitoringSelection::ILLUMINATION);

        let (_, kind, delayed) = schedule
            .due(started + Duration::from_secs(10))
            .expect("one delayed combined sample");
        assert_eq!(kind, "illumination+slow");
        assert!(delayed.illumination);
        assert!(delayed.temperature);
        assert!(delayed.pixel_dead_time);
        assert!(
            schedule.due(started + Duration::from_secs(10)).is_none(),
            "a delayed poll must not trigger a catch-up burst"
        );
    }

    #[test]
    fn sensor_telemetry_csv_exposes_poll_and_raw_offset_brackets() {
        let mut csv = Vec::new();
        write_sensor_telemetry_sample(
            &mut csv,
            &SensorTelemetrySample {
                sample_id: 4,
                poll_kind: "illumination",
                host_elapsed_start_us: 1_000,
                host_elapsed_end_us: 1_025,
                raw_data_offset_before_bytes: 65_536,
                raw_data_offset_after_bytes: 131_072,
                values: SensorMonitoring {
                    illumination_lux: Some(12.5),
                    ..SensorMonitoring::default()
                },
                status: "valid",
                error: None,
            },
        )
        .expect("sample serializes");
        let csv = String::from_utf8(csv).expect("CSV is UTF-8");
        assert_eq!(
            csv,
            "1,4,illumination,1000,1025,65536,131072,12.5,,,,,,,,valid,\n"
        );
    }

    #[test]
    fn recording_writer_refuses_to_overwrite_existing_file() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("augur-create-new-{nanos}.raw"));
        std::fs::write(&path, b"keep").expect("collision fixture is written");

        prepare_output_writer(&path, false, 1, 1, 1024, None)
            .expect_err("existing recording target must not be overwritten");
        assert_eq!(std::fs::read(&path).expect("fixture remains"), b"keep");

        let _ = std::fs::remove_file(path);
    }

    #[derive(Default)]
    struct TimeoutCamera;

    impl EventCamera for TimeoutCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }
    }

    impl PacketStreamCamera for TimeoutCamera {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("idle test camera".into()))
        }
    }

    #[test]
    fn telemetry_rejects_inline_camera_without_leaving_output_files() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        let raw_path = std::env::temp_dir().join(format!("augur-telemetry-inline-{nanos}.raw"));
        let telemetry_path = sensor_telemetry_path(&raw_path).expect("companion path");
        let config_path = recording_config_path(&raw_path).expect("config path");
        let mut options = PipelineOptions::new(&raw_path);
        options.sensor_telemetry = Some(SensorTelemetryOptions::default());

        let error = match spawn_pipeline(
            TimeoutCamera,
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            options,
        ) {
            Ok(controller) => {
                let _ = controller.shutdown();
                panic!("inline camera cannot safely record control telemetry");
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("independent control thread"));
        assert!(!raw_path.exists());
        assert!(!telemetry_path.exists());
        assert!(!config_path.exists());
    }

    struct ScriptedPacketCamera {
        packets: Vec<Vec<u8>>,
        next_packet: usize,
        release_after_first: Option<Arc<AtomicBool>>,
    }

    impl EventCamera for ScriptedPacketCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }
    }

    impl PacketStreamCamera for ScriptedPacketCamera {
        fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
            if self.next_packet >= self.packets.len() {
                return Err(CameraError::Eof);
            }
            if self.next_packet > 0
                && self
                    .release_after_first
                    .as_ref()
                    .is_some_and(|gate| !gate.load(Ordering::Relaxed))
            {
                return Err(CameraError::Timeout("waiting for test gate".into()));
            }

            let packet = &self.packets[self.next_packet];
            buf[..packet.len()].copy_from_slice(packet);
            self.next_packet += 1;
            Ok(packet.len())
        }
    }

    struct TaggedEventDecoder {
        tagged_events: Vec<(u8, Vec<CdEvent>)>,
    }

    impl TaggedEventDecoder {
        fn new(tagged_events: Vec<(u8, Vec<CdEvent>)>) -> Self {
            Self { tagged_events }
        }
    }

    impl PreviewDecoder for TaggedEventDecoder {
        fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()> {
            out.clear();
            let Some(tag) = bytes.first().copied() else {
                return Ok(());
            };
            if let Some((_, events)) = self
                .tagged_events
                .iter()
                .find(|(candidate, _)| *candidate == tag)
            {
                out.extend_from_slice(events);
            }
            Ok(())
        }
    }

    fn recv_preview_frame(controller: &PipelineController) -> PreviewFrame {
        controller
            .frame_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("preview frame must arrive")
    }

    fn test_event(timestamp: u64, x: u16) -> CdEvent {
        CdEvent {
            x,
            y: 0,
            timestamp,
            polarity: true,
        }
    }

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
            raw_pool_starvation_events: 0,
            raw_pool_starvation_us: 0,
            raw_pool_starvation_max_us: 0,
            stop_drain_packets: 0,
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
            triggers_total: 0,
            triggers_dropped: 0,
            last_trigger_at: None,
            recent_samples: VecDeque::new(),
        }
    }

    #[test]
    fn trigger_counter_accumulates_edges_and_records_last_edge_age() {
        let start = Instant::now();
        let mut stats = test_stats(start);

        // No edges yet: count is zero and there is no "last edge" age.
        let snapshot = stats.snapshot_at(start);
        assert_eq!(snapshot.triggers_total, 0);
        assert_eq!(snapshot.last_trigger_age_s, None);

        // A zero-count call must not stamp a "last edge" time.
        stats.record_triggers(start + Duration::from_millis(10), 0);
        assert_eq!(stats.snapshot_at(start).triggers_total, 0);
        assert_eq!(stats.snapshot_at(start).last_trigger_age_s, None);

        // Rising + falling edges both count toward the decode-level total.
        stats.record_triggers(start + Duration::from_millis(100), 2);
        stats.record_triggers(start + Duration::from_millis(300), 3);

        let snapshot = stats.snapshot_at(start + Duration::from_millis(800));
        assert_eq!(snapshot.triggers_total, 5);
        // Age is measured from the most recent edge (300 ms) to now (800 ms).
        let age = snapshot.last_trigger_age_s.expect("an edge was recorded");
        assert!((age - 0.5).abs() < 1e-6, "unexpected age: {age}");
    }

    #[test]
    fn pending_triggers_stay_bounded_when_no_cd_events_close_a_window() {
        let mut pending = Vec::new();
        let mut stats = test_stats(Instant::now());

        // Under the cap nothing is touched: these edges still annotate a frame.
        for timestamp_us in 0..1_000u64 {
            pending.push(ExternalTriggerEvent::new(timestamp_us, 0, true));
        }
        assert_eq!(bound_pending_triggers(&mut pending), 0);
        assert_eq!(pending.len(), 1_000);

        // A CD-free stream keeps appending; only the recent tail is kept.
        for timestamp_us in 1_000..(MAX_PENDING_TRIGGERS as u64 + 5_000) {
            pending.push(ExternalTriggerEvent::new(timestamp_us, 0, true));
        }
        let dropped = bound_pending_triggers(&mut pending);
        assert_eq!(dropped, 5_000);
        assert_eq!(pending.len(), MAX_PENDING_TRIGGERS);
        assert_eq!(
            pending.first().map(|trigger| trigger.timestamp_us),
            Some(5_000),
            "the oldest edges must be the ones dropped"
        );

        stats.record_dropped_triggers(dropped);
        assert_eq!(
            stats.snapshot_at(Instant::now()).triggers_dropped,
            5_000,
            "drops must be observable rather than silent"
        );
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
    fn decodes_external_triggers_with_unwrapped_timestamps() {
        let words = [
            (0x8 << 12) | 0x001,        // TIME_HIGH
            (0x6 << 12) | 0x002,        // TIME_LOW -> t = (1<<12)|2
            (0xA << 12) | (3 << 8) | 1, // EXT_TRIGGER rising, id=3
            (0x6 << 12) | 0x004,        // TIME_LOW -> t = (1<<12)|4
            (0xA << 12) | (3 << 8),     // EXT_TRIGGER falling, id=3
        ];
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        let mut triggers = Vec::new();

        decoder
            .decode_bytes(&words_to_bytes(&words), &mut events)
            .expect("decoder must succeed");
        decoder.take_triggers(&mut triggers);

        assert!(events.is_empty());
        assert_eq!(triggers.len(), 2);
        assert_eq!(triggers[0].timestamp_us, (1_u64 << 12) | 2);
        assert_eq!(triggers[0].id, 3);
        assert!(triggers[0].is_rising());
        assert_eq!(triggers[1].timestamp_us, (1_u64 << 12) | 4);
        assert!(!triggers[1].is_rising());

        let mut drained_again = Vec::new();
        decoder.take_triggers(&mut drained_again);
        assert!(drained_again.is_empty(), "take_triggers must drain");
    }

    #[test]
    fn assigns_triggers_to_the_cd_epoch_across_rollover_straddle() {
        let period = crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US;
        // Chunk 1: CD event just before the 24-bit rollover.
        let first_chunk =
            words_to_bytes(&[(0x8 << 12) | 0xFFF, (0x6 << 12) | 0xFF0, 8, (0x2 << 12) | 4]);
        // Chunk 2: post-rollover trigger + CD event, raw timestamps small again.
        let second_chunk = words_to_bytes(&[
            (0x8 << 12),
            (0x6 << 12) | 0x010,
            (0xA << 12) | 1, // EXT_TRIGGER rising, id=0, raw t = 0x10
            8,
            (0x2 << 12) | 5,
        ]);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        let mut triggers = Vec::new();

        decoder
            .decode_bytes(&first_chunk, &mut events)
            .expect("first chunk must decode");
        decoder
            .decode_bytes(&second_chunk, &mut events)
            .expect("second chunk must decode");
        decoder.take_triggers(&mut triggers);

        assert_eq!(triggers.len(), 1);
        assert_eq!(
            triggers[0].timestamp_us,
            period + 16,
            "the trigger must land in the post-rollover epoch, not epoch 0"
        );
    }

    #[test]
    fn assigns_trigger_only_packet_to_the_cd_epoch_after_rollover() {
        let period = crate::evt3_timestamps::EVT3_TIMESTAMP_PERIOD_US;
        // CD stream parks just before the rollover, then a trigger-only
        // packet arrives after the wrap with no CD event to re-anchor it.
        let cd_chunk =
            words_to_bytes(&[(0x8 << 12) | 0xFFF, (0x6 << 12) | 0xFF0, 8, (0x2 << 12) | 4]);
        let trigger_only_chunk =
            words_to_bytes(&[(0x8 << 12), (0x6 << 12) | 0x010, (0xA << 12) | 1]);
        let mut decoder = Evt3CorePreviewDecoder::default();
        let mut events = Vec::new();
        let mut triggers = Vec::new();

        decoder
            .decode_bytes(&cd_chunk, &mut events)
            .expect("cd chunk must decode");
        decoder
            .decode_bytes(&trigger_only_chunk, &mut events)
            .expect("trigger chunk must decode");
        decoder.take_triggers(&mut triggers);

        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].timestamp_us, period + 16);
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
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
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

    #[test]
    fn pipeline_controller_uses_initial_config_acquisition_time() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 123;

        let controller = spawn_pipeline(
            TimeoutCamera,
            Evt3CorePreviewDecoder::default(),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        assert_eq!(controller.acq_time_us.load(Ordering::Relaxed), 123_000);
        controller.shutdown().expect("pipeline must shut down");
    }

    #[test]
    fn preview_frame_skips_upstream_source_when_raw_events_are_not_required() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1;

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![vec![1]],
                next_packet: 0,
                release_after_first: None,
            },
            TaggedEventDecoder::new(vec![(1, vec![test_event(0, 1), test_event(1_000, 2)])]),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        let frame = recv_preview_frame(&controller);

        assert!(frame.events.is_none());
        assert_eq!(frame.event_range, None);
        assert_eq!(frame.events_snapshot(), None);
        assert_eq!(frame.compact_events_snapshot(), None);
        assert_eq!(controller.event_source.next_event_idx(), 0);

        controller.shutdown().expect("shutdown must succeed");
    }

    #[test]
    fn preview_pipeline_excludes_out_of_bounds_events_from_counts_without_raw_retention() {
        let mut config = CameraConfig::default();
        config.roi.width = 4;
        config.roi.height = 4;
        config.global.acq_time_ms = 1;
        let valid_events = [test_event(1_000, 1), test_event(2_000, 2)];

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![vec![1]],
                next_packet: 0,
                release_after_first: None,
            },
            TaggedEventDecoder::new(vec![(
                1,
                vec![
                    CdEvent {
                        x: 99,
                        y: 0,
                        timestamp: 0,
                        polarity: true,
                    },
                    valid_events[0],
                    valid_events[1],
                ],
            )]),
            config,
            PipelineOptions::preview_only(4, 4),
        )
        .expect("pipeline must start");

        let frame = recv_preview_frame(&controller);

        assert_eq!(frame.window_start_us, 1_000);
        assert_eq!(frame.window_end_us, 2_000);
        assert_eq!(frame.on_count + frame.off_count, 2);
        assert_eq!(frame.events_snapshot(), None);
        assert_eq!(controller.event_source.next_event_idx(), 0);

        controller.shutdown().expect("shutdown must succeed");
    }

    #[test]
    fn live_event_source_drains_lossless_cursor_by_frame() {
        let source = LiveEventSource::with_capacity(4);
        let cursor = source.register_cursor(
            "plugin:test",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );
        source
            .append_cd_frame(&[test_event(10, 1), test_event(20, 2)], 10, 20)
            .expect("first frame fits");
        source
            .append_cd_frame(&[test_event(30, 3)], 30, 30)
            .expect("second frame fits");

        let batches = source
            .drain_cursor_frames(cursor)
            .expect("cursor drain succeeds");

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].event_range, 0..2);
        assert_eq!(
            batches[0].events,
            vec![
                CompactEvent::from(test_event(10, 1)),
                CompactEvent::from(test_event(20, 2))
            ]
        );
        assert_eq!(batches[1].event_range, 2..3);
        assert_eq!(
            source
                .drain_cursor_frames(cursor)
                .expect("second drain succeeds"),
            Vec::new()
        );

        source
            .append_cd_frame(&[test_event(40, 4), test_event(50, 5)], 40, 50)
            .expect("advanced cursor permits eviction");
    }

    #[test]
    fn preview_frame_drop_skips_archival_when_raw_events_are_not_required() {
        let (frame_tx, frame_rx) = bounded(1);
        let mut buffers = take_preview_frame_buffers(4);
        let mut events = vec![test_event(10, 1)];
        let mut on_count = 1;
        let mut off_count = 0;
        let mut frame_start_ts = Some(10);
        let stats = Arc::new(Mutex::new(test_stats(Instant::now())));
        let source = LiveEventSource::default();
        frame_tx
            .send(PreviewFrame {
                width: 2,
                height: 2,
                pixels: Vec::new(),
                pixels_on: Vec::new(),
                pixels_off: Vec::new(),
                cached_total_histogram: Vec::new(),
                cached_signed_histogram: Vec::new(),
                on_count: 0,
                off_count: 0,
                events: None,
                event_range: None,
                event_source: None,
                external_triggers: Vec::new(),
                window_start_us: 0,
                window_end_us: 0,
            })
            .expect("queue seed succeeds");

        emit_preview_frame(
            &frame_tx,
            &stats,
            &source,
            None,
            &mut buffers,
            &mut events,
            &mut Vec::new(),
            false,
            &mut on_count,
            &mut off_count,
            &mut frame_start_ts,
            2,
            2,
            4,
            10,
        );

        assert_eq!(frame_rx.len(), 1);
        assert_eq!(source.next_event_idx(), 0);
        assert_eq!(stats.lock().expect("stats").preview_frame_drops, 1);
    }

    #[test]
    fn preview_frame_drop_keeps_events_in_upstream_source_when_raw_events_are_required() {
        let (frame_tx, frame_rx) = bounded(1);
        let mut buffers = take_preview_frame_buffers(4);
        let mut events = vec![test_event(10, 1)];
        let mut on_count = 1;
        let mut off_count = 0;
        let mut frame_start_ts = Some(10);
        let stats = Arc::new(Mutex::new(test_stats(Instant::now())));
        let source = LiveEventSource::default();
        frame_tx
            .send(PreviewFrame {
                width: 2,
                height: 2,
                pixels: Vec::new(),
                pixels_on: Vec::new(),
                pixels_off: Vec::new(),
                cached_total_histogram: Vec::new(),
                cached_signed_histogram: Vec::new(),
                on_count: 0,
                off_count: 0,
                events: None,
                event_range: None,
                event_source: None,
                external_triggers: Vec::new(),
                window_start_us: 0,
                window_end_us: 0,
            })
            .expect("queue seed succeeds");

        emit_preview_frame(
            &frame_tx,
            &stats,
            &source,
            None,
            &mut buffers,
            &mut events,
            &mut Vec::new(),
            true,
            &mut on_count,
            &mut off_count,
            &mut frame_start_ts,
            2,
            2,
            4,
            10,
        );

        assert_eq!(frame_rx.len(), 1);
        assert_eq!(source.events_for_range(0..1), Some(vec![test_event(10, 1)]));
        assert_eq!(stats.lock().expect("stats").preview_frame_drops, 1);
    }

    #[test]
    fn oversized_frame_keeps_preview_alive_without_ring_archival() {
        // A single accumulation window larger than the ring capacity must not
        // tear down the preview: the frame is still emitted (with no ring-backed
        // event range) and the archival miss is counted as a drop.
        let (frame_tx, frame_rx) = bounded(1);
        let mut buffers = take_preview_frame_buffers(4);
        // Ring holds a single event; the frame carries two -> FrameTooLarge.
        let source = LiveEventSource::with_capacity(1);
        let mut events = vec![test_event(10, 0), test_event(20, 1)];
        let mut on_count = 2;
        let mut off_count = 0;
        let mut frame_start_ts = Some(10);
        let stats = Arc::new(Mutex::new(test_stats(Instant::now())));

        emit_preview_frame(
            &frame_tx,
            &stats,
            &source,
            None,
            &mut buffers,
            &mut events,
            &mut Vec::new(),
            true,
            &mut on_count,
            &mut off_count,
            &mut frame_start_ts,
            2,
            2,
            4,
            20,
        );

        let frame = frame_rx.try_recv().expect("preview frame is still emitted");
        assert_eq!(frame.event_range, None, "oversized frame has no ring range");
        assert_eq!(
            frame.events.as_deref(),
            Some(&[test_event(10, 0), test_event(20, 1)][..]),
            "captured raw events still travel with the frame",
        );
        assert_eq!(source.next_event_idx(), 0, "nothing was stored in the ring");
        assert_eq!(stats.lock().expect("stats").preview_frame_drops, 1);
        // Accumulators are reset so the next window starts clean.
        assert_eq!(frame_start_ts, None);
        assert_eq!(on_count, 0);
        assert_eq!(off_count, 0);
    }

    struct CountingStreamReader {
        reads: Arc<AtomicU64>,
    }

    impl crate::camera::PacketStreamReader for CountingStreamReader {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(1));
            Err(CameraError::Timeout("idle counting reader".into()))
        }
    }

    /// Split-capable camera whose runtime reconfigure waits until the stream
    /// reader has made progress, proving reads continue during configure.
    struct SplitControlCamera {
        reads: Arc<AtomicU64>,
        configure_calls: u32,
        reads_progressed_during_reconfigure: Arc<AtomicU64>,
        reconfigured: Arc<AtomicBool>,
    }

    impl EventCamera for SplitControlCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            self.configure_calls += 1;
            if self.configure_calls == 1 {
                return Ok(());
            }
            let reads_at_start = self.reads.load(Ordering::Relaxed);
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                let progressed = self
                    .reads
                    .load(Ordering::Relaxed)
                    .saturating_sub(reads_at_start);
                if progressed >= 3 {
                    self.reads_progressed_during_reconfigure
                        .store(progressed, Ordering::Relaxed);
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            self.reconfigured.store(true, Ordering::Relaxed);
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }
    }

    impl PacketStreamCamera for SplitControlCamera {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("split camera reads via reader".into()))
        }

        fn split_stream_reader(&mut self) -> Option<Box<dyn crate::camera::PacketStreamReader>> {
            Some(Box::new(CountingStreamReader {
                reads: Arc::clone(&self.reads),
            }))
        }
    }

    #[test]
    fn split_camera_keeps_reading_during_runtime_reconfigure() {
        let reads = Arc::new(AtomicU64::new(0));
        let progressed = Arc::new(AtomicU64::new(0));
        let reconfigured = Arc::new(AtomicBool::new(false));

        let controller = spawn_pipeline(
            SplitControlCamera {
                reads: Arc::clone(&reads),
                configure_calls: 0,
                reads_progressed_during_reconfigure: Arc::clone(&progressed),
                reconfigured: Arc::clone(&reconfigured),
            },
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        controller
            .settings_tx
            .send(CameraConfig::default())
            .expect("settings channel must accept the runtime config");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !reconfigured.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "runtime reconfigure never ran");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            progressed.load(Ordering::Relaxed) >= 3,
            "stream reads must continue while the control thread reconfigures",
        );

        controller.shutdown().expect("pipeline must shut down");
    }

    /// Camera whose monitoring readback is scripted per call, so a test can
    /// drive the success, empty, and failure paths in order.
    struct ScriptedMonitoringCamera {
        /// A reading, or the message the readback should fail with. `CameraError`
        /// is not `Clone`, so the script stores the message instead.
        readings: Vec<std::result::Result<SensorMonitoring, String>>,
        calls: usize,
    }

    impl ScriptedMonitoringCamera {
        fn new(readings: Vec<std::result::Result<SensorMonitoring, String>>) -> Self {
            Self { readings, calls: 0 }
        }
    }

    impl EventCamera for ScriptedMonitoringCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }

        fn read_monitoring(&mut self) -> Result<SensorMonitoring> {
            let reading = self.readings.get(self.calls).cloned();
            self.calls += 1;
            match reading {
                Some(Ok(values)) => Ok(values),
                Some(Err(message)) => Err(CameraError::Transport(message)),
                None => Ok(SensorMonitoring::default()),
            }
        }
    }

    fn dead_time_reading(us: f32) -> SensorMonitoring {
        SensorMonitoring {
            pixel_dead_time_us: Some(us),
            ..SensorMonitoring::default()
        }
    }

    #[test]
    fn monitoring_poll_is_skipped_until_a_consumer_asks() {
        let mut camera = ScriptedMonitoringCamera::new(vec![Ok(dead_time_reading(6.0))]);
        let needed = AtomicBool::new(false);
        let state = Mutex::new(None);
        let mut last_poll = None;

        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);

        assert_eq!(camera.calls, 0, "an unasked-for readback must cost nothing");
        assert!(state.lock().expect("state lock").is_none());
    }

    #[test]
    fn monitoring_poll_publishes_and_then_rate_limits() {
        let mut camera = ScriptedMonitoringCamera::new(vec![
            Ok(dead_time_reading(6.0)),
            Ok(dead_time_reading(9.0)),
        ]);
        let needed = AtomicBool::new(true);
        let state = Mutex::new(None);
        let mut last_poll = None;

        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);
        // Immediately again: inside the interval, so it must not touch the bus.
        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);

        assert_eq!(camera.calls, 1, "polls inside the interval must be skipped");
        let published = state
            .lock()
            .expect("state lock")
            .as_ref()
            .expect("a reading must be published")
            .snapshot_at(Instant::now());
        assert_eq!(published.values.pixel_dead_time_us, Some(6.0));
        assert!(published.error.is_none());
    }

    #[test]
    fn monitoring_poll_drops_the_reading_once_nobody_asks() {
        let mut camera = ScriptedMonitoringCamera::new(vec![Ok(dead_time_reading(6.0))]);
        let needed = AtomicBool::new(true);
        let state = Mutex::new(None);
        let mut last_poll = None;

        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);
        assert!(state.lock().expect("state lock").is_some());

        needed.store(false, Ordering::Relaxed);
        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);

        assert!(
            state.lock().expect("state lock").is_none(),
            "a closed consumer must not keep a reading that is no longer refreshed",
        );
        assert!(last_poll.is_none());
    }

    #[test]
    fn monitoring_poll_publishes_nothing_for_a_source_without_monitoring() {
        let mut camera = ScriptedMonitoringCamera::new(vec![Ok(SensorMonitoring::default())]);
        let needed = AtomicBool::new(true);
        let state = Mutex::new(None);
        let mut last_poll = None;

        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);

        assert_eq!(camera.calls, 1);
        assert!(
            state.lock().expect("state lock").is_none(),
            "an all-None reading must not look like available live values",
        );
    }

    #[test]
    fn monitoring_poll_keeps_the_last_good_values_when_a_read_fails() {
        let mut camera = ScriptedMonitoringCamera::new(vec![
            Ok(dead_time_reading(6.0)),
            Err("register read timed out".into()),
        ]);
        let needed = AtomicBool::new(true);
        let state = Mutex::new(None);
        let mut last_poll = None;

        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);
        let first_at = state
            .lock()
            .expect("state lock")
            .as_ref()
            .expect("first reading")
            .at;

        // Force the interval open so the second, failing poll runs.
        last_poll = None;
        poll_sensor_monitoring(&mut camera, &needed, &state, &mut last_poll);

        let guard = state.lock().expect("state lock");
        let stored = guard.as_ref().expect("state must survive a failed read");
        assert_eq!(
            stored.values.pixel_dead_time_us,
            Some(6.0),
            "a failed read must not erase the last successful values",
        );
        assert_eq!(
            stored.at, first_at,
            "the timestamp must stay put so the reading visibly ages",
        );
        assert!(stored
            .error
            .as_deref()
            .is_some_and(|error| error.contains("register read timed out")));
    }

    #[test]
    fn split_camera_publishes_monitoring_only_while_requested() {
        let controller = spawn_pipeline(
            MonitoringStreamCamera,
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        // Nothing asked yet: the control thread must not publish anything.
        thread::sleep(Duration::from_millis(150));
        assert!(controller.sensor_monitoring().is_none());

        controller
            .sensor_monitoring_needed
            .store(true, Ordering::Relaxed);

        let deadline = Instant::now() + Duration::from_secs(5);
        let snapshot = loop {
            if let Some(snapshot) = controller.sensor_monitoring() {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "control thread never published a monitoring reading",
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(snapshot.values.pixel_dead_time_us, Some(6.0));

        controller.shutdown().expect("pipeline must shut down");
    }

    /// Split-capable camera that reports a fixed dead time, for the
    /// control-thread wiring test.
    struct MonitoringStreamCamera;

    impl EventCamera for MonitoringStreamCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }

        fn read_monitoring(&mut self) -> Result<SensorMonitoring> {
            Ok(dead_time_reading(6.0))
        }
    }

    impl PacketStreamCamera for MonitoringStreamCamera {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("split camera reads via reader".into()))
        }

        fn split_stream_reader(&mut self) -> Option<Box<dyn crate::camera::PacketStreamReader>> {
            Some(Box::new(CountingStreamReader {
                reads: Arc::new(AtomicU64::new(0)),
            }))
        }
    }

    struct SelectiveTelemetryCamera {
        selections: Arc<Mutex<Vec<SensorMonitoringSelection>>>,
    }

    impl EventCamera for SelectiveTelemetryCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }

        fn read_monitoring_selected(
            &mut self,
            selection: SensorMonitoringSelection,
        ) -> Result<SensorMonitoring> {
            self.selections.lock().expect("selections").push(selection);
            Ok(SensorMonitoring {
                pixel_dead_time_us: selection.pixel_dead_time.then_some(6.25),
                illumination_lux: selection.illumination.then_some(42.0),
                temperature_c: selection.temperature.then_some(31.5),
                biases: None,
            })
        }
    }

    impl PacketStreamCamera for SelectiveTelemetryCamera {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("split camera reads via reader".into()))
        }

        fn split_stream_reader(&mut self) -> Option<Box<dyn crate::camera::PacketStreamReader>> {
            Some(Box::new(CountingStreamReader {
                reads: Arc::new(AtomicU64::new(0)),
            }))
        }
    }

    #[test]
    fn telemetry_pipeline_records_selected_samples_and_finalizes_sidecar() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("system clock is valid")
            .as_nanos();
        let raw_path = std::env::temp_dir().join(format!("augur-telemetry-{nanos}.raw"));
        let telemetry_path = sensor_telemetry_path(&raw_path).expect("companion path");
        let config_path = recording_config_path(&raw_path).expect("config path");
        let selections = Arc::new(Mutex::new(Vec::new()));
        let mut options = PipelineOptions::new(&raw_path);
        options.sensor_telemetry = Some(SensorTelemetryOptions {
            illumination_interval: Duration::from_millis(20),
            slow_interval: Duration::from_millis(60),
        });

        let controller = spawn_pipeline(
            SelectiveTelemetryCamera {
                selections: Arc::clone(&selections),
            },
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            options,
        )
        .expect("telemetry pipeline starts");
        let deadline = Instant::now() + Duration::from_secs(2);
        while controller.sensor_telemetry().samples_written < 3 {
            assert!(
                Instant::now() < deadline,
                "telemetry samples were not written"
            );
            thread::sleep(Duration::from_millis(5));
        }
        controller.shutdown().expect("pipeline shuts down");

        let selections = selections.lock().expect("selections");
        assert!(selections
            .iter()
            .any(|selection| selection.illumination && selection.temperature));
        assert!(selections
            .iter()
            .any(|selection| selection.illumination && !selection.temperature));
        let csv = std::fs::read_to_string(&telemetry_path).expect("telemetry CSV");
        assert!(csv.lines().count() >= 4);
        assert!(csv.contains(",42,31.5,6.25,"));
        let sidecar = std::fs::read_to_string(&config_path).expect("recording sidecar");
        assert!(sidecar.contains("sensor_monitoring_samples_written"));
        assert!(sidecar.contains("host_monotonic_with_raw_data_offset_bracket"));

        let _ = std::fs::remove_file(raw_path);
        let _ = std::fs::remove_file(telemetry_path);
        let _ = std::fs::remove_file(config_path);
    }

    /// Camera whose second packet only completes after the test requests
    /// stop, like a USB bulk transfer that lands just as shutdown begins.
    struct StopGatedTailCamera {
        sent_first: bool,
        tail_sent: bool,
        stop_requested: Arc<AtomicBool>,
        /// Set once the camera is blocked inside the tail read, so the test can
        /// request the stop while that read is genuinely in flight instead of
        /// racing the stream thread's stop-flag check.
        in_tail_read: Arc<AtomicBool>,
    }

    impl EventCamera for StopGatedTailCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }
    }

    impl PacketStreamCamera for StopGatedTailCamera {
        fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
            if !self.sent_first {
                self.sent_first = true;
                buf[..4].copy_from_slice(&[1, 2, 3, 4]);
                return Ok(4);
            }
            if self.tail_sent {
                return Err(CameraError::Eof);
            }
            self.in_tail_read.store(true, Ordering::Relaxed);
            let waited = Instant::now();
            while !self.stop_requested.load(Ordering::Relaxed) {
                if waited.elapsed() > Duration::from_secs(10) {
                    return Err(CameraError::Timeout("stop gate never opened".into()));
                }
                thread::sleep(Duration::from_millis(1));
            }
            // Stop is now requested while this read is still in flight. Give
            // the disk thread ample time to observe the stop flag before the
            // packet is handed over, so any stop-flag-based disk exit would
            // lose this tail packet.
            thread::sleep(Duration::from_millis(80));
            self.tail_sent = true;
            buf[..4].copy_from_slice(&[5, 6, 7, 8]);
            Ok(4)
        }
    }

    #[test]
    fn disk_writer_persists_packet_accepted_after_stop_request() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must be sane")
            .as_nanos();
        let output_path = std::env::temp_dir().join(format!("augur-tail-loss-{nanos}.raw"));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let in_tail_read = Arc::new(AtomicBool::new(false));

        let mut options = PipelineOptions::new(&output_path);
        options.write_evt3_header = false;
        let controller = spawn_pipeline(
            StopGatedTailCamera {
                sent_first: false,
                tail_sent: false,
                stop_requested: Arc::clone(&stop_requested),
                in_tail_read: Arc::clone(&in_tail_read),
            },
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            options,
        )
        .expect("pipeline must start");

        // Wait until the first packet has been accepted by the USB reader and
        // the camera is blocked inside the gated second read. Observing the
        // read itself (not just the first packet's byte count) keeps the stop
        // request from racing the stream thread's stop-flag check.
        let deadline = Instant::now() + Duration::from_secs(10);
        while !in_tail_read.load(Ordering::Relaxed) {
            assert!(Instant::now() < deadline, "tail read never started");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(controller.stats_snapshot().bytes_total >= 4);

        controller.request_stop();
        stop_requested.store(true, Ordering::Relaxed);
        controller.shutdown().expect("pipeline must shut down");

        let written = std::fs::read(&output_path).expect("recording file must exist");
        let _ = std::fs::remove_file(&output_path);
        let _ = std::fs::remove_file(output_path.with_extension("toml"));
        assert_eq!(
            written,
            vec![1, 2, 3, 4, 5, 6, 7, 8],
            "every packet accepted by the USB reader must reach the recording",
        );
    }

    /// Stream reader that delivers one packet, then holds the rest as data it
    /// already received but has not handed over — exactly the state the async
    /// multi-URB reader is in when a recording is stopped with transfers
    /// completed in its queue.
    struct BufferedTailReader {
        delivered_first: bool,
        buffered: VecDeque<Vec<u8>>,
    }

    impl crate::camera::PacketStreamReader for BufferedTailReader {
        fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
            if !self.delivered_first {
                self.delivered_first = true;
                buf[..4].copy_from_slice(&[1, 2, 3, 4]);
                return Ok(4);
            }
            // No *new* data arrives; the tail only exists in the reader's own
            // completed-transfer queue.
            thread::sleep(Duration::from_millis(1));
            Err(CameraError::Timeout("no new packet".into()))
        }

        fn take_buffered_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
            match self.buffered.pop_front() {
                Some(packet) => {
                    buf[..packet.len()].copy_from_slice(&packet);
                    Ok(packet.len())
                }
                None => Ok(0),
            }
        }
    }

    struct BufferedTailCamera {
        reader: Option<Box<dyn crate::camera::PacketStreamReader>>,
    }

    impl EventCamera for BufferedTailCamera {
        fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
            Ok(())
        }

        fn start_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn stop_streaming(&mut self) -> Result<()> {
            Ok(())
        }

        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::default()
        }
    }

    impl PacketStreamCamera for BufferedTailCamera {
        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("split camera reads via reader".into()))
        }

        fn split_stream_reader(&mut self) -> Option<Box<dyn crate::camera::PacketStreamReader>> {
            self.reader.take()
        }
    }

    #[test]
    fn packets_already_received_by_the_reader_reach_the_recording_after_stop() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must be sane")
            .as_nanos();
        let output_path = std::env::temp_dir().join(format!("augur-reader-tail-{nanos}.raw"));

        let mut options = PipelineOptions::new(&output_path);
        options.write_evt3_header = false;
        let controller = spawn_pipeline(
            BufferedTailCamera {
                reader: Some(Box::new(BufferedTailReader {
                    delivered_first: false,
                    buffered: VecDeque::from(vec![vec![5, 6, 7, 8], vec![9, 10, 11, 12]]),
                })),
            },
            Evt3CorePreviewDecoder::default(),
            CameraConfig::default(),
            options,
        )
        .expect("pipeline must start");

        let deadline = Instant::now() + Duration::from_secs(10);
        while controller.stats_snapshot().bytes_total < 4 {
            assert!(Instant::now() < deadline, "first packet never arrived");
            thread::sleep(Duration::from_millis(1));
        }

        controller.request_stop();
        controller.shutdown().expect("pipeline must shut down");

        let written = std::fs::read(&output_path).expect("recording file must exist");
        let _ = std::fs::remove_file(&output_path);
        let _ = std::fs::remove_file(output_path.with_extension("toml"));
        assert_eq!(
            written,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            "data the device already delivered to the host must not be dropped at stop",
        );
    }

    struct ServiceCountingWorker {
        services: u64,
    }

    impl StreamWorker for ServiceCountingWorker {
        fn start(&mut self) -> std::result::Result<(), String> {
            Ok(())
        }

        fn poll_control(&mut self, _error_tx: &Sender<String>) {}

        fn read_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Err(CameraError::Timeout("unused".into()))
        }

        fn service(&mut self, _budget: Duration) {
            self.services += 1;
        }

        fn take_buffered_packet(&mut self, _buf: &mut [u8]) -> Result<usize> {
            Ok(0)
        }

        fn finish(&mut self) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn empty_buffer_pool_keeps_the_transport_serviced_and_is_recorded() {
        // Keep the sender alive: a disconnected pool is a different path.
        let (_pool_tx, pool_rx) = bounded::<UsbBuffer>(4);
        let stop = AtomicBool::new(false);
        let stats = Mutex::new(PipelineStatsInner::new());
        let mut worker = ServiceCountingWorker { services: 0 };

        let outcome = acquire_raw_buffer(&mut worker, &stop, &stats, &pool_rx);

        assert!(matches!(outcome, RawBufferWait::Timeout));
        assert!(
            worker.services > 0,
            "queued transfers must keep being serviced while the pipeline cannot accept data",
        );
        let snapshot = stats.lock().expect("stats").snapshot();
        assert_eq!(snapshot.raw_pool_starvation_events, 1);
        assert!(
            snapshot.raw_pool_starvation_max_us > 0,
            "the stall duration must be measurable",
        );
        assert!(
            snapshot.recording_may_have_gaps(),
            "a stalled recording path must be reported as a possible gap",
        );
    }

    #[test]
    fn an_available_raw_buffer_is_taken_without_reporting_a_stall() {
        let (pool_tx, pool_rx) = bounded::<UsbBuffer>(4);
        pool_tx
            .send(Box::new([0_u8; BUF_SIZE]))
            .expect("pool must accept the buffer");
        let stop = AtomicBool::new(false);
        let stats = Mutex::new(PipelineStatsInner::new());
        let mut worker = ServiceCountingWorker { services: 0 };

        let outcome = acquire_raw_buffer(&mut worker, &stop, &stats, &pool_rx);

        assert!(matches!(outcome, RawBufferWait::Buffer(_)));
        assert_eq!(worker.services, 0);
        assert!(!stats
            .lock()
            .expect("stats")
            .snapshot()
            .recording_may_have_gaps());
    }

    #[test]
    fn preview_pipeline_splits_multiple_frames_within_one_packet() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1;

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![vec![1]],
                next_packet: 0,
                release_after_first: None,
            },
            TaggedEventDecoder::new(vec![(
                1,
                vec![
                    CdEvent {
                        x: 0,
                        y: 0,
                        timestamp: 0,
                        polarity: true,
                    },
                    CdEvent {
                        x: 1,
                        y: 0,
                        timestamp: 600,
                        polarity: false,
                    },
                    CdEvent {
                        x: 2,
                        y: 0,
                        timestamp: 1_200,
                        polarity: true,
                    },
                    CdEvent {
                        x: 3,
                        y: 0,
                        timestamp: 1_800,
                        polarity: false,
                    },
                    CdEvent {
                        x: 4,
                        y: 0,
                        timestamp: 2_400,
                        polarity: true,
                    },
                    CdEvent {
                        x: 5,
                        y: 0,
                        timestamp: 3_000,
                        polarity: false,
                    },
                ],
            )]),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        let first = recv_preview_frame(&controller);
        let second = recv_preview_frame(&controller);

        assert_eq!(first.window_start_us, 0);
        assert_eq!(first.window_end_us, 1_200);
        assert_eq!(first.on_count + first.off_count, 3);
        assert_eq!(second.window_start_us, 1_800);
        assert_eq!(second.window_end_us, 3_000);
        assert_eq!(second.on_count + second.off_count, 3);
        assert!(
            controller
                .frame_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "partial trailing windows should not emit a frame"
        );

        controller.shutdown().expect("pipeline must shut down");
    }

    #[test]
    fn preview_pipeline_applies_runtime_acquisition_time_to_later_packets() {
        let release_after_first = Arc::new(AtomicBool::new(false));
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1;

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![vec![1], vec![2]],
                next_packet: 0,
                release_after_first: Some(Arc::clone(&release_after_first)),
            },
            TaggedEventDecoder::new(vec![
                (
                    1,
                    vec![
                        CdEvent {
                            x: 0,
                            y: 0,
                            timestamp: 0,
                            polarity: true,
                        },
                        CdEvent {
                            x: 1,
                            y: 0,
                            timestamp: 600,
                            polarity: false,
                        },
                        CdEvent {
                            x: 2,
                            y: 0,
                            timestamp: 1_200,
                            polarity: true,
                        },
                    ],
                ),
                (
                    2,
                    vec![
                        CdEvent {
                            x: 3,
                            y: 0,
                            timestamp: 2_000,
                            polarity: false,
                        },
                        CdEvent {
                            x: 4,
                            y: 0,
                            timestamp: 2_600,
                            polarity: true,
                        },
                        CdEvent {
                            x: 5,
                            y: 0,
                            timestamp: 3_200,
                            polarity: false,
                        },
                        CdEvent {
                            x: 6,
                            y: 0,
                            timestamp: 3_800,
                            polarity: true,
                        },
                        CdEvent {
                            x: 7,
                            y: 0,
                            timestamp: 4_400,
                            polarity: false,
                        },
                        CdEvent {
                            x: 8,
                            y: 0,
                            timestamp: 5_000,
                            polarity: true,
                        },
                        CdEvent {
                            x: 9,
                            y: 0,
                            timestamp: 5_600,
                            polarity: false,
                        },
                    ],
                ),
            ]),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        let first = recv_preview_frame(&controller);
        assert_eq!(first.on_count + first.off_count, 3);

        controller.acq_time_us.store(2_000, Ordering::Relaxed);
        release_after_first.store(true, Ordering::Relaxed);

        let second = recv_preview_frame(&controller);
        assert_eq!(second.window_start_us, 2_000);
        assert_eq!(second.window_end_us, 4_400);
        assert_eq!(second.on_count + second.off_count, 5);
        assert!(
            controller
                .frame_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "the remaining events stay below the larger acquisition window"
        );

        controller.shutdown().expect("pipeline must shut down");
    }

    #[test]
    fn preview_pipeline_assigns_triggers_to_frame_windows() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1;

        // One packet: CD at t=0, trigger at t=600, CD at t=1200 closes the
        // 1 ms window; the trigger must ride inside that frame.
        let packet = words_to_bytes(&[
            (0x8 << 12),         // TIME_HIGH 0
            (0x6 << 12),         // TIME_LOW 0
            8,                   // ADDR_Y
            (0x2 << 12) | 4,     // CD event t=0
            (0x6 << 12) | 600,   // TIME_LOW 600
            (0xA << 12) | 1,     // trigger rising, id=0, t=600
            (0x6 << 12) | 1_200, // TIME_LOW 1200
            8,                   // ADDR_Y
            (0x2 << 12) | 5,     // CD event t=1200
        ]);

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![packet],
                next_packet: 0,
                release_after_first: None,
            },
            Evt3CorePreviewDecoder::default(),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        let frame = recv_preview_frame(&controller);
        assert_eq!(frame.window_start_us, 0);
        assert_eq!(frame.window_end_us, 1_200);
        assert_eq!(frame.on_count + frame.off_count, 2);
        assert_eq!(frame.external_triggers.len(), 1);
        assert_eq!(frame.external_triggers[0].timestamp_us, 600);
        assert!(frame.external_triggers[0].is_rising());

        controller.shutdown().expect("pipeline must shut down");
    }

    #[test]
    fn preview_pipeline_flushes_trigger_only_edges_at_eof() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1;

        // No CD events at all: triggers do not force frames mid-stream in the
        // annotations-only preview, but they must never be silently dropped —
        // the EOF flush emits a single frame carrying every pending edge.
        let packet = words_to_bytes(&[
            (0x8 << 12),         // TIME_HIGH 0
            (0x6 << 12) | 100,   // TIME_LOW 100
            (0xA << 12) | 1,     // trigger rising t=100
            (0x6 << 12) | 200,   // TIME_LOW 200
            (0xA << 12),         // trigger falling t=200
            (0x6 << 12) | 1_500, // TIME_LOW 1500
            (0xA << 12) | 1,     // trigger rising t=1500
        ]);

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![packet],
                next_packet: 0,
                release_after_first: None,
            },
            Evt3CorePreviewDecoder::default(),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        // A single EOF-flushed frame carries all three edges; there is no
        // mid-stream trigger-forced frame anymore.
        let flushed = recv_preview_frame(&controller);
        assert_eq!(flushed.window_start_us, 100);
        assert_eq!(flushed.on_count + flushed.off_count, 0);
        assert_eq!(flushed.external_triggers.len(), 3);
        assert!(flushed.external_triggers[0].is_rising());
        assert!(!flushed.external_triggers[1].is_rising());
        assert_eq!(flushed.external_triggers[2].timestamp_us, 1_500);

        assert!(
            controller
                .frame_rx
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "no mid-stream trigger-forced frames"
        );

        controller.shutdown().expect("pipeline must shut down");
    }

    // Regression (preview flicker): a trigger sitting more than one acquisition
    // window ahead of a sparse CD stream must NOT force an extra empty (black)
    // frame. The edges ride along on the CD-driven frame instead. Previously the
    // trigger-close loop emitted a black frame per acq step up to the edge,
    // which showed up as flicker whenever external triggers were enabled.
    #[test]
    fn triggers_ride_along_without_forcing_empty_frames() {
        let mut config = CameraConfig::default();
        config.global.acq_time_ms = 1; // 1 ms accumulation window

        // One CD event at t=0, then two triggers at t=1500 and t=2500 with no
        // CD events near them (sparse scene between edges).
        let packet = words_to_bytes(&[
            (0x8 << 12),         // TIME_HIGH 0
            (0x6 << 12),         // TIME_LOW 0
            8,                   // ADDR_Y
            (0x2 << 12) | 4,     // CD event t=0
            (0x6 << 12) | 1_500, // TIME_LOW 1500
            (0xA << 12) | 1,     // trigger rising t=1500
            (0x6 << 12) | 2_500, // TIME_LOW 2500
            (0xA << 12) | 1,     // trigger rising t=2500
        ]);

        let controller = spawn_pipeline(
            ScriptedPacketCamera {
                packets: vec![packet],
                next_packet: 0,
                release_after_first: None,
            },
            Evt3CorePreviewDecoder::default(),
            config,
            PipelineOptions::preview_only(1280, 720),
        )
        .expect("pipeline must start");

        // Exactly one frame: it carries the CD content AND both edges as
        // annotations — no separate black frame.
        let frame = recv_preview_frame(&controller);
        assert_eq!(
            frame.on_count + frame.off_count,
            1,
            "frame carries CD content"
        );
        assert_eq!(frame.external_triggers.len(), 2, "both edges ride along");

        assert!(
            controller
                .frame_rx
                .recv_timeout(Duration::from_millis(200))
                .is_err(),
            "no empty trigger-forced frame is emitted"
        );

        controller.shutdown().expect("pipeline must shut down");
    }
}
