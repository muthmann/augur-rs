use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::Path,
    sync::{atomic::Ordering, Arc},
    thread,
    time::{Duration, Instant},
};

use augur_event_types::{CompactEvent, EventChunk, EventSource, FetchError};

use crate::{
    camera::{DeviceInfo, EventCamera, PacketStreamCamera},
    config::CameraConfig,
    metadata::RecordingMetadata,
    pipeline::{CdEvent, PreviewDecoder},
    replay::{ReplayControls, ReplayFileInfo},
    CameraError, Result,
};

#[cfg(feature = "hdf5")]
use hdf5::types::{VarLenAscii, VarLenUnicode};

const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const THROTTLE_SLEEP_SLICE: Duration = Duration::from_millis(10);
const BIN_MAGIC: &[u8; 8] = b"EVT3BIN\0";
const BIN_HEADER_BYTES: usize = 28;
const NPY_MAGIC: &[u8; 6] = b"\x93NUMPY";
const NPY_RECORD_BYTES: usize = 16;
const DEFAULT_NPY_WIDTH: u32 = 1280;
const DEFAULT_NPY_HEIGHT: u32 = 720;
pub const PACKED_EVENT_RECORD_BYTES: usize = 14;

#[cfg(feature = "hdf5")]
#[derive(hdf5::H5Type, Clone)]
#[repr(C)]
struct RawHdf5CdEvent {
    x: u16,
    y: u16,
    p: i16,
    t: i64,
}

#[derive(Debug)]
pub struct DecodedEventFileCamera {
    events: Arc<Vec<CdEvent>>,
    event_idx: usize,
    info: DeviceInfo,
    controls: ReplayControls,
    session_start_ts_us: u64,
    last_speed_epoch: u32,
    playback_started_at: Option<Instant>,
    paused_at: Option<Instant>,
    total_paused: Duration,
}

#[derive(Debug, Clone)]
pub struct DecodedReplayEventSource {
    events: Arc<Vec<CdEvent>>,
}

impl DecodedReplayEventSource {
    pub fn new(events: Arc<Vec<CdEvent>>) -> Self {
        Self { events }
    }

    pub fn events(&self) -> &Arc<Vec<CdEvent>> {
        &self.events
    }
}

impl EventSource for DecodedReplayEventSource {
    fn fetch_range(
        &self,
        start_us: u64,
        end_us: u64,
    ) -> std::result::Result<EventChunk, FetchError> {
        if start_us > end_us {
            return Err(FetchError::OutOfTimeline);
        }
        let start = self
            .events
            .partition_point(|event| event.timestamp < start_us);
        let end = self
            .events
            .partition_point(|event| event.timestamp <= end_us);
        if start >= end {
            return Err(FetchError::OutOfTimeline);
        }

        Ok(EventChunk {
            events: self.events[start..end]
                .iter()
                .copied()
                .map(CompactEvent::from)
                .collect(),
            // Decoded imports (CSV/HDF5/packed) carry no trigger channel in
            // v1; only EVT3 RAW sources deliver external triggers.
            triggers: Vec::new(),
            start_us,
            end_us,
        })
    }
}

impl DecodedEventFileCamera {
    pub fn open(
        path: impl AsRef<Path>,
    ) -> Result<(Self, ReplayControls, ReplayFileInfo, Arc<Vec<CdEvent>>)> {
        let path = path.as_ref();
        let (events, width, height) = match replay_extension(path)?.as_str() {
            "csv" => parse_csv(path)?,
            "bin" => parse_bin(path)?,
            "npy" => parse_npy(path)?,
            #[cfg(feature = "hdf5")]
            "h5" | "hdf5" => parse_hdf5(path)?,
            #[cfg(not(feature = "hdf5"))]
            "h5" | "hdf5" => {
                return Err(CameraError::Config(
                    "HDF5 replay support is not compiled in; rebuild with `--features hdf5`".into(),
                ))
            }
            ext => {
                return Err(CameraError::Config(format!(
                    "unsupported decoded replay extension: .{ext}"
                )))
            }
        };

        let events = Arc::new(events);
        let total_bytes = events.len() as u64 * PACKED_EVENT_RECORD_BYTES as u64;
        let first_timestamp_us = events.first().map(|event| event.timestamp).unwrap_or(0);
        let total_duration_us = events
            .last()
            .map(|event| event.timestamp)
            .unwrap_or(first_timestamp_us)
            .saturating_sub(first_timestamp_us);
        let nominal_bytes_per_sec = (total_duration_us > 0)
            .then_some(total_bytes as f64 / (total_duration_us as f64 / 1_000_000.0));

        let info = ReplayFileInfo {
            file_size: total_bytes,
            data_offset: 0,
            width,
            height,
            metadata: RecordingMetadata::default(),
            total_duration_us,
            first_timestamp_us,
            nominal_bytes_per_sec,
        };
        let (camera, controls) = Self::open_at(events.clone(), &info, 0)?;
        Ok((camera, controls, info, events))
    }

    pub fn open_at(
        events: Arc<Vec<CdEvent>>,
        info: &ReplayFileInfo,
        start_byte: u64,
    ) -> Result<(Self, ReplayControls)> {
        let start_event_idx = ((start_byte.saturating_sub(info.data_offset))
            / PACKED_EVENT_RECORD_BYTES as u64)
            .min(events.len() as u64) as usize;
        let start_bytes = start_event_idx as u64 * PACKED_EVENT_RECORD_BYTES as u64;
        let controls = ReplayControls::new(info, start_bytes);
        controls.current_timestamp_us.store(
            events
                .get(start_event_idx)
                .or_else(|| events.last())
                .map(|event| event.timestamp)
                .unwrap_or(info.first_timestamp_us),
            Ordering::Relaxed,
        );

        Ok((
            Self {
                session_start_ts_us: events
                    .get(start_event_idx)
                    .or_else(|| events.last())
                    .map(|event| event.timestamp)
                    .unwrap_or(info.first_timestamp_us),
                events,
                event_idx: start_event_idx,
                info: decoded_device_info(),
                controls: controls.clone(),
                last_speed_epoch: controls.speed_epoch.load(Ordering::Relaxed),
                playback_started_at: None,
                paused_at: None,
                total_paused: Duration::ZERO,
            },
            controls,
        ))
    }

    fn mark_paused(&mut self, now: Instant) {
        if self.paused_at.is_none() {
            self.paused_at = Some(now);
        }
    }

    fn mark_resumed(&mut self, now: Instant) {
        if let Some(paused_at) = self.paused_at.take() {
            self.total_paused += now.saturating_duration_since(paused_at);
        }
        self.playback_started_at.get_or_insert(now);
    }

    fn speed_multiplier(&self) -> f32 {
        f32::from_bits(self.controls.speed_bits.load(Ordering::Relaxed))
    }

    fn current_event_timestamp_us(&self) -> u64 {
        self.events
            .get(self.event_idx)
            .or_else(|| self.events.last())
            .map(|event| event.timestamp)
            .unwrap_or(self.controls.first_timestamp_us)
    }

    fn reset_timing_baseline(&mut self, now: Instant, current_ts: u64) {
        self.session_start_ts_us = current_ts;
        self.playback_started_at = Some(now);
        self.total_paused = Duration::ZERO;
        self.paused_at = None;
    }

    fn reset_throttle_baseline_if_needed(&mut self, now: Instant) {
        let epoch = self.controls.speed_epoch.load(Ordering::Relaxed);
        if epoch == self.last_speed_epoch {
            return;
        }

        self.last_speed_epoch = epoch;
        let current_ts = self.current_event_timestamp_us();
        self.reset_timing_baseline(now, current_ts);
    }

    fn throttle_to_current_progress(&mut self) -> Result<()> {
        let speed = self.speed_multiplier();
        if !speed.is_finite() || speed <= 0.0 {
            return Ok(());
        }

        let now = Instant::now();
        self.reset_throttle_baseline_if_needed(now);

        let current_ts = self.current_event_timestamp_us();
        let event_elapsed_us = current_ts.saturating_sub(self.session_start_ts_us);
        if event_elapsed_us == 0 {
            return Ok(());
        }

        let started_at = *self.playback_started_at.get_or_insert(now);
        loop {
            let now = Instant::now();
            if self.controls.paused.load(Ordering::Relaxed) {
                self.mark_paused(now);
                thread::sleep(PAUSE_POLL_INTERVAL);
                return Err(CameraError::Timeout("replay paused".into()));
            }

            self.mark_resumed(now);
            let active_elapsed = now
                .saturating_duration_since(started_at)
                .saturating_sub(self.total_paused);
            let desired_elapsed_s = event_elapsed_us as f64 / (speed as f64 * 1_000_000.0);
            let remaining_s = desired_elapsed_s - active_elapsed.as_secs_f64();
            if remaining_s <= 0.0 {
                break;
            }

            thread::sleep(Duration::from_secs_f64(
                remaining_s.min(THROTTLE_SLEEP_SLICE.as_secs_f64()),
            ));
        }

        Ok(())
    }
}

impl EventCamera for DecodedEventFileCamera {
    fn configure(&mut self, _config: &CameraConfig) -> Result<()> {
        Ok(())
    }

    fn start_streaming(&mut self) -> Result<()> {
        self.playback_started_at.get_or_insert_with(Instant::now);
        Ok(())
    }

    fn stop_streaming(&mut self) -> Result<()> {
        Ok(())
    }

    fn device_info(&self) -> DeviceInfo {
        self.info.clone()
    }
}

impl PacketStreamCamera for DecodedEventFileCamera {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        let now = Instant::now();
        if self.controls.paused.load(Ordering::Relaxed) {
            self.mark_paused(now);
            thread::sleep(PAUSE_POLL_INTERVAL);
            return Err(CameraError::Timeout("replay paused".into()));
        }

        let events_per_packet = buf.len() / PACKED_EVENT_RECORD_BYTES;
        if events_per_packet == 0 {
            return Err(CameraError::Config(
                "decoded replay packet buffer is smaller than one event record".into(),
            ));
        }

        if self.event_idx >= self.events.len() {
            return Err(CameraError::Eof);
        }

        self.mark_resumed(now);
        self.throttle_to_current_progress()?;

        let remaining = &self.events[self.event_idx..];
        let events_to_write = remaining.len().min(events_per_packet);
        let bytes_to_write = events_to_write * PACKED_EVENT_RECORD_BYTES;
        for (chunk, event) in buf[..bytes_to_write]
            .chunks_exact_mut(PACKED_EVENT_RECORD_BYTES)
            .zip(remaining.iter().take(events_to_write))
        {
            encode_packed_event(*event, chunk);
        }

        self.event_idx += events_to_write;
        self.controls.current_timestamp_us.store(
            remaining
                .get(events_to_write.saturating_sub(1))
                .map(|event| event.timestamp)
                .unwrap_or(self.controls.first_timestamp_us),
            Ordering::Relaxed,
        );
        self.controls.bytes_read.store(
            self.event_idx as u64 * PACKED_EVENT_RECORD_BYTES as u64,
            Ordering::Relaxed,
        );
        Ok(bytes_to_write)
    }
}

#[derive(Debug, Default)]
pub struct PackedEventPreviewDecoder {
    partial: [u8; PACKED_EVENT_RECORD_BYTES],
    partial_len: usize,
}

impl PreviewDecoder for PackedEventPreviewDecoder {
    fn decode_bytes(&mut self, bytes: &[u8], out: &mut Vec<CdEvent>) -> Result<()> {
        out.clear();
        let mut remaining = bytes;

        if self.partial_len > 0 {
            let needed = PACKED_EVENT_RECORD_BYTES - self.partial_len;
            let copied = needed.min(remaining.len());
            self.partial[self.partial_len..self.partial_len + copied]
                .copy_from_slice(&remaining[..copied]);
            self.partial_len += copied;
            remaining = &remaining[copied..];

            if self.partial_len == PACKED_EVENT_RECORD_BYTES {
                out.push(decode_packed_event(&self.partial));
                self.partial_len = 0;
            }
        }

        let mut chunks = remaining.chunks_exact(PACKED_EVENT_RECORD_BYTES);
        out.reserve(chunks.len());
        for chunk in chunks.by_ref() {
            out.push(decode_packed_event(chunk));
        }

        let remainder = chunks.remainder();
        if !remainder.is_empty() {
            self.partial[..remainder.len()].copy_from_slice(remainder);
            self.partial_len = remainder.len();
        }

        Ok(())
    }

    fn finish_stream(&mut self) -> Result<()> {
        self.partial_len = 0;
        Ok(())
    }

    fn estimate_event_count(bytes: &[u8]) -> u64 {
        (bytes.len() / PACKED_EVENT_RECORD_BYTES) as u64
    }
}

fn replay_extension(path: &Path) -> Result<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or_else(|| {
            CameraError::Config(format!(
                "replay file {} is missing a supported extension",
                path.display()
            ))
        })
}

fn decoded_device_info() -> DeviceInfo {
    DeviceInfo {
        vendor: "AugurRS".into(),
        model: "Decoded Event Replay".into(),
        serial: None,
        firmware: None,
        compatible: Some("decoded-event file".into()),
    }
}

fn encode_packed_event(event: CdEvent, out: &mut [u8]) {
    out[..2].copy_from_slice(&event.x.to_le_bytes());
    out[2..4].copy_from_slice(&event.y.to_le_bytes());
    out[4] = u8::from(event.polarity);
    out[5] = 0;
    out[6..14].copy_from_slice(&event.timestamp.to_le_bytes());
}

fn decode_packed_event(bytes: &[u8]) -> CdEvent {
    CdEvent {
        x: u16::from_le_bytes([bytes[0], bytes[1]]),
        y: u16::from_le_bytes([bytes[2], bytes[3]]),
        polarity: bytes[4] != 0,
        timestamp: u64::from_le_bytes([
            bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13],
        ]),
    }
}

fn parse_csv(path: &Path) -> Result<(Vec<CdEvent>, u16, u16)> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut geometry = None;
    let mut events = Vec::new();

    for (line_idx, line) in reader.lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(raw_geometry) = line.strip_prefix("%geometry:") {
            geometry = Some(parse_csv_geometry(raw_geometry, path)?);
            continue;
        }
        if line.starts_with('%') {
            continue;
        }

        let mut fields = line.split(',');
        let x = parse_csv_u16(fields.next(), "x", line_idx + 1, path)?;
        let y = parse_csv_u16(fields.next(), "y", line_idx + 1, path)?;
        let polarity = parse_csv_u8(fields.next(), "polarity", line_idx + 1, path)?;
        let timestamp = parse_csv_u64(fields.next(), "timestamp", line_idx + 1, path)?;
        if fields.next().is_some() {
            return Err(CameraError::Config(format!(
                "csv replay file {} line {} has more than 4 columns",
                path.display(),
                line_idx + 1
            )));
        }

        events.push(CdEvent {
            x,
            y,
            polarity: polarity != 0,
            timestamp,
        });
    }

    let (width, height) = geometry.ok_or_else(|| {
        CameraError::Config(format!(
            "csv replay file {} is missing the required %geometry:W,H header",
            path.display()
        ))
    })?;

    Ok((events, width, height))
}

fn parse_bin(path: &Path) -> Result<(Vec<CdEvent>, u16, u16)> {
    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();
    let mut header = [0_u8; BIN_HEADER_BYTES];
    file.read_exact(&mut header)?;

    if &header[..8] != BIN_MAGIC {
        return Err(CameraError::Config(format!(
            "binary replay file {} has an invalid magic header",
            path.display()
        )));
    }

    let version = u32::from_le_bytes(header[8..12].try_into().expect("version bytes"));
    if version != 1 {
        return Err(CameraError::Config(format!(
            "binary replay file {} uses unsupported version {version}",
            path.display()
        )));
    }

    let width = parse_geometry_u16(
        "width",
        u32::from_le_bytes(header[12..16].try_into().expect("width bytes")),
        path,
    )?;
    let height = parse_geometry_u16(
        "height",
        u32::from_le_bytes(header[16..20].try_into().expect("height bytes")),
        path,
    )?;
    let event_count = u64::from_le_bytes(header[20..28].try_into().expect("count bytes"));
    let expected_size = BIN_HEADER_BYTES as u64
        + event_count
            .checked_mul(PACKED_EVENT_RECORD_BYTES as u64)
            .ok_or_else(|| {
                CameraError::Config(format!(
                    "binary replay file {} declares too many events",
                    path.display()
                ))
            })?;
    if file_size != expected_size {
        return Err(CameraError::Config(format!(
            "binary replay file {} size mismatch: header expects {expected_size} bytes, file has {file_size}",
            path.display()
        )));
    }

    let event_count = usize::try_from(event_count).map_err(|_| {
        CameraError::Config(format!(
            "binary replay file {} has too many events for this platform",
            path.display()
        ))
    })?;
    let mut events = Vec::with_capacity(event_count);
    let mut record = [0_u8; PACKED_EVENT_RECORD_BYTES];
    for _ in 0..event_count {
        file.read_exact(&mut record)?;
        events.push(decode_packed_event(&record));
    }

    Ok((events, width, height))
}

fn parse_npy(path: &Path) -> Result<(Vec<CdEvent>, u16, u16)> {
    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();

    let mut magic = [0_u8; 6];
    file.read_exact(&mut magic)?;
    if &magic != NPY_MAGIC {
        return Err(CameraError::Config(format!(
            "npy replay file {} has an invalid magic header",
            path.display()
        )));
    }

    let mut version = [0_u8; 2];
    file.read_exact(&mut version)?;
    let header_len_size = if version[0] == 1 { 2_u64 } else { 4_u64 };
    let header_len = match version[0] {
        1 => {
            let mut raw = [0_u8; 2];
            file.read_exact(&mut raw)?;
            u16::from_le_bytes(raw) as u64
        }
        2 | 3 => {
            let mut raw = [0_u8; 4];
            file.read_exact(&mut raw)?;
            u32::from_le_bytes(raw) as u64
        }
        major => {
            return Err(CameraError::Config(format!(
                "npy replay file {} uses unsupported major version {major}",
                path.display()
            )))
        }
    };

    let mut header_bytes = vec![0_u8; header_len as usize];
    file.read_exact(&mut header_bytes)?;
    let header = std::str::from_utf8(&header_bytes).map_err(|err| {
        CameraError::Config(format!(
            "npy replay file {} has a non-UTF-8 header: {err}",
            path.display()
        ))
    })?;
    validate_npy_header(path, header)?;
    let event_count = parse_npy_event_count(path, header)?;

    let expected_size = NPY_MAGIC.len() as u64
        + 2
        + header_len_size
        + header_len
        + event_count
            .checked_mul(NPY_RECORD_BYTES as u64)
            .ok_or_else(|| {
                CameraError::Config(format!(
                    "npy replay file {} declares too many events",
                    path.display()
                ))
            })?;
    if file_size != expected_size {
        return Err(CameraError::Config(format!(
            "npy replay file {} size mismatch: header expects {expected_size} bytes, file has {file_size}",
            path.display()
        )));
    }

    let event_count = usize::try_from(event_count).map_err(|_| {
        CameraError::Config(format!(
            "npy replay file {} has too many events for this platform",
            path.display()
        ))
    })?;
    let mut events = Vec::with_capacity(event_count);
    let mut record = [0_u8; NPY_RECORD_BYTES];
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    for _ in 0..event_count {
        file.read_exact(&mut record)?;
        let x = u16::from_le_bytes([record[0], record[1]]);
        let y = u16::from_le_bytes([record[2], record[3]]);
        let polarity = i16::from_le_bytes([record[4], record[5]]) != 0;
        let timestamp = i64::from_le_bytes([
            record[8], record[9], record[10], record[11], record[12], record[13], record[14],
            record[15],
        ]);
        let timestamp = u64::try_from(timestamp).map_err(|_| {
            CameraError::Config(format!(
                "npy replay file {} contains a negative timestamp",
                path.display()
            ))
        })?;

        max_x = max_x.max(x as u32);
        max_y = max_y.max(y as u32);
        events.push(CdEvent {
            x,
            y,
            polarity,
            timestamp,
        });
    }

    let width = parse_geometry_u16(
        "width",
        (max_x.saturating_add(1)).max(DEFAULT_NPY_WIDTH),
        path,
    )?;
    let height = parse_geometry_u16(
        "height",
        (max_y.saturating_add(1)).max(DEFAULT_NPY_HEIGHT),
        path,
    )?;

    Ok((events, width, height))
}

#[cfg(feature = "hdf5")]
fn parse_hdf5(path: &Path) -> Result<(Vec<CdEvent>, u16, u16)> {
    let file = hdf5::File::open(path).map_err(|err| {
        CameraError::Config(format!(
            "hdf5 replay file {} could not be opened: {err}",
            path.display()
        ))
    })?;

    let geometry = parse_hdf5_geometry(&file, path)?;
    let raw_events = file
        .group("CD")
        .map_err(|_| {
            CameraError::Config(format!(
                "hdf5 replay file {} is missing the CD group",
                path.display()
            ))
        })?
        .dataset("events")
        .map_err(|_| {
            CameraError::Config(format!(
                "hdf5 replay file {} is missing the CD/events dataset",
                path.display()
            ))
        })?
        .read_1d::<RawHdf5CdEvent>()
        .map_err(|err| {
            CameraError::Config(format!(
                "hdf5 replay file {} failed to read CD/events: {err}",
                path.display()
            ))
        })?;

    let mut events = Vec::with_capacity(raw_events.len());
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    for raw in raw_events {
        let timestamp = u64::try_from(raw.t).map_err(|_| {
            CameraError::Config(format!(
                "hdf5 replay file {} contains a negative timestamp {}",
                path.display(),
                raw.t
            ))
        })?;
        max_x = max_x.max(raw.x as u32);
        max_y = max_y.max(raw.y as u32);
        events.push(CdEvent {
            x: raw.x,
            y: raw.y,
            polarity: raw.p > 0,
            timestamp,
        });
    }

    let width = parse_geometry_u16(
        "width",
        geometry
            .map(|(width, _)| width)
            .unwrap_or_else(|| (max_x.saturating_add(1)).max(DEFAULT_NPY_WIDTH)),
        path,
    )?;
    let height = parse_geometry_u16(
        "height",
        geometry
            .map(|(_, height)| height)
            .unwrap_or_else(|| (max_y.saturating_add(1)).max(DEFAULT_NPY_HEIGHT)),
        path,
    )?;

    Ok((events, width, height))
}

fn parse_csv_geometry(raw: &str, path: &Path) -> Result<(u16, u16)> {
    let (raw_width, raw_height) = raw.split_once(',').ok_or_else(|| {
        CameraError::Config(format!(
            "csv replay file {} has invalid geometry header {raw:?}",
            path.display()
        ))
    })?;
    Ok((
        parse_geometry_u16(
            "width",
            raw_width.trim().parse().map_err(|err| {
                CameraError::Config(format!(
                    "csv replay file {} has invalid geometry width {:?}: {err}",
                    path.display(),
                    raw_width.trim()
                ))
            })?,
            path,
        )?,
        parse_geometry_u16(
            "height",
            raw_height.trim().parse().map_err(|err| {
                CameraError::Config(format!(
                    "csv replay file {} has invalid geometry height {:?}: {err}",
                    path.display(),
                    raw_height.trim()
                ))
            })?,
            path,
        )?,
    ))
}

fn parse_csv_u16(raw: Option<&str>, label: &str, line: usize, path: &Path) -> Result<u16> {
    raw.ok_or_else(|| {
        CameraError::Config(format!(
            "csv replay file {} line {line} is missing {label}",
            path.display()
        ))
    })?
    .trim()
    .parse::<u16>()
    .map_err(|err| {
        CameraError::Config(format!(
            "csv replay file {} line {line} has invalid {label}: {err}",
            path.display()
        ))
    })
}

fn parse_csv_u8(raw: Option<&str>, label: &str, line: usize, path: &Path) -> Result<u8> {
    raw.ok_or_else(|| {
        CameraError::Config(format!(
            "csv replay file {} line {line} is missing {label}",
            path.display()
        ))
    })?
    .trim()
    .parse::<u8>()
    .map_err(|err| {
        CameraError::Config(format!(
            "csv replay file {} line {line} has invalid {label}: {err}",
            path.display()
        ))
    })
}

fn parse_csv_u64(raw: Option<&str>, label: &str, line: usize, path: &Path) -> Result<u64> {
    raw.ok_or_else(|| {
        CameraError::Config(format!(
            "csv replay file {} line {line} is missing {label}",
            path.display()
        ))
    })?
    .trim()
    .parse::<u64>()
    .map_err(|err| {
        CameraError::Config(format!(
            "csv replay file {} line {line} has invalid {label}: {err}",
            path.display()
        ))
    })
}

fn parse_geometry_u16(label: &str, raw: u32, path: &Path) -> Result<u16> {
    u16::try_from(raw).map_err(|_| {
        CameraError::Config(format!(
            "{} replay file {} has {label} {raw} that exceeds u16 geometry limits",
            replay_extension(path).unwrap_or_else(|_| "decoded".into()),
            path.display()
        ))
    })
}

#[cfg(feature = "hdf5")]
fn parse_hdf5_geometry(file: &hdf5::File, path: &Path) -> Result<Option<(u32, u32)>> {
    let attr = match file.attr("geometry") {
        Ok(attr) => attr,
        Err(_) => return Ok(None),
    };

    let geometry = attr
        .read_scalar::<VarLenAscii>()
        .map(|geometry| geometry.as_str().to_string())
        .or_else(|_| {
            attr.read_scalar::<VarLenUnicode>()
                .map(|geometry| geometry.to_string())
        })
        .map_err(|err| {
            CameraError::Config(format!(
                "hdf5 replay file {} has an unreadable geometry attribute: {err}",
                path.display()
            ))
        })?;
    let geometry = geometry.as_str();
    let Some((raw_width, raw_height)) = geometry.split_once('x') else {
        return Err(CameraError::Config(format!(
            "hdf5 replay file {} geometry attribute {:?} is not in WxH format",
            path.display(),
            geometry
        )));
    };

    Ok(Some((
        raw_width.trim().parse::<u32>().map_err(|err| {
            CameraError::Config(format!(
                "hdf5 replay file {} has invalid geometry width {:?}: {err}",
                path.display(),
                raw_width.trim()
            ))
        })?,
        raw_height.trim().parse::<u32>().map_err(|err| {
            CameraError::Config(format!(
                "hdf5 replay file {} has invalid geometry height {:?}: {err}",
                path.display(),
                raw_height.trim()
            ))
        })?,
    )))
}

fn validate_npy_header(path: &Path, header: &str) -> Result<()> {
    let normalized: String = header.chars().filter(|ch| !ch.is_whitespace()).collect();
    if !normalized.contains("'fortran_order':False") {
        return Err(CameraError::Config(format!(
            "npy replay file {} must use fortran_order=False",
            path.display()
        )));
    }
    if !normalized.contains("'descr':[('x','<u2'),('y','<u2'),('p','<i2'),('','|V2'),('t','<i8')]")
    {
        return Err(CameraError::Config(format!(
            "npy replay file {} has an unsupported dtype; expected evt3-core structured XYPT records",
            path.display()
        )));
    }
    Ok(())
}

fn parse_npy_event_count(path: &Path, header: &str) -> Result<u64> {
    let normalized: String = header.chars().filter(|ch| !ch.is_whitespace()).collect();
    let marker = "'shape':(";
    let shape_start = normalized.find(marker).ok_or_else(|| {
        CameraError::Config(format!(
            "npy replay file {} is missing a shape entry",
            path.display()
        ))
    })? + marker.len();
    let shape_end = normalized[shape_start..].find(')').ok_or_else(|| {
        CameraError::Config(format!(
            "npy replay file {} has an unterminated shape entry",
            path.display()
        ))
    })? + shape_start;
    let raw_shape = &normalized[shape_start..shape_end];
    let event_count = raw_shape.trim_end_matches(',');
    event_count.parse::<u64>().map_err(|err| {
        CameraError::Config(format!(
            "npy replay file {} has unsupported shape ({raw_shape}): {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        env, fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[cfg(feature = "hdf5")]
    use hdf5::types::{VarLenAscii, VarLenUnicode};
    #[cfg(feature = "hdf5")]
    use std::str::FromStr;

    fn temp_path(suffix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after epoch")
            .as_nanos();
        env::temp_dir().join(format!("augur-decoded-replay-{nanos}.{suffix}"))
    }

    fn sample_events() -> Vec<CdEvent> {
        vec![
            CdEvent {
                x: 10,
                y: 20,
                polarity: true,
                timestamp: 100,
            },
            CdEvent {
                x: 11,
                y: 21,
                polarity: false,
                timestamp: 120,
            },
            CdEvent {
                x: 12,
                y: 22,
                polarity: true,
                timestamp: 150,
            },
        ]
    }

    fn write_csv(path: &Path, width: u16, height: u16, events: &[CdEvent]) {
        let mut body = format!("%geometry:{width},{height}\n");
        for event in events {
            body.push_str(&format!(
                "{},{},{},{}\n",
                event.x,
                event.y,
                u8::from(event.polarity),
                event.timestamp
            ));
        }
        fs::write(path, body).expect("csv sample must be written");
    }

    fn write_bin(path: &Path, width: u16, height: u16, events: &[CdEvent]) {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(BIN_MAGIC);
        bytes.extend_from_slice(&1_u32.to_le_bytes());
        bytes.extend_from_slice(&(width as u32).to_le_bytes());
        bytes.extend_from_slice(&(height as u32).to_le_bytes());
        bytes.extend_from_slice(&(events.len() as u64).to_le_bytes());
        for event in events {
            let mut record = [0_u8; PACKED_EVENT_RECORD_BYTES];
            encode_packed_event(*event, &mut record);
            bytes.extend_from_slice(&record);
        }
        fs::write(path, bytes).expect("bin sample must be written");
    }

    fn write_npy(path: &Path, events: &[CdEvent]) {
        let raw_header = format!(
            "{{'descr': [('x', '<u2'), ('y', '<u2'), ('p', '<i2'), ('', '|V2'), ('t', '<i8')], 'fortran_order': False, 'shape': ({},), }}",
            events.len()
        );
        let mut header = raw_header.into_bytes();
        let preamble_len = NPY_MAGIC.len() + 2 + 2;
        while !(preamble_len + header.len() + 1).is_multiple_of(16) {
            header.push(b' ');
        }
        header.push(b'\n');

        let mut bytes = Vec::new();
        bytes.extend_from_slice(NPY_MAGIC);
        bytes.push(1);
        bytes.push(0);
        bytes.extend_from_slice(&(header.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&header);
        for event in events {
            bytes.extend_from_slice(&event.x.to_le_bytes());
            bytes.extend_from_slice(&event.y.to_le_bytes());
            bytes.extend_from_slice(&(if event.polarity { 1_i16 } else { 0_i16 }).to_le_bytes());
            bytes.extend_from_slice(&[0, 0]);
            bytes.extend_from_slice(&(event.timestamp as i64).to_le_bytes());
        }
        fs::write(path, bytes).expect("npy sample must be written");
    }

    #[cfg(feature = "hdf5")]
    fn write_hdf5(path: &Path, geometry: Option<&str>, events: &[RawHdf5CdEvent]) {
        let file = hdf5::File::create(path).expect("hdf5 sample must be created");

        if let Some(geometry) = geometry {
            let geometry =
                VarLenUnicode::from_str(geometry).expect("geometry string must be valid");
            file.new_attr::<VarLenUnicode>()
                .shape(())
                .create("geometry")
                .expect("geometry attribute must be created")
                .write_scalar(&geometry)
                .expect("geometry attribute must be written");
        }

        let group = file.create_group("CD").expect("CD group must be created");
        group
            .new_dataset_builder()
            .with_data(events)
            .create("events")
            .expect("CD events dataset must be created");
    }

    #[cfg(feature = "hdf5")]
    fn write_hdf5_ascii(path: &Path, geometry: Option<&str>, events: &[RawHdf5CdEvent]) {
        let file = hdf5::File::create(path).expect("hdf5 sample must be created");

        if let Some(geometry) = geometry {
            let geometry = VarLenAscii::from_ascii(geometry.as_bytes())
                .expect("geometry string must be ASCII");
            file.new_attr::<VarLenAscii>()
                .shape(())
                .create("geometry")
                .expect("geometry attribute must be created")
                .write_scalar(&geometry)
                .expect("geometry attribute must be written");
        }

        let group = file.create_group("CD").expect("CD group must be created");
        group
            .new_dataset_builder()
            .with_data(events)
            .create("events")
            .expect("CD events dataset must be created");
    }

    #[test]
    fn packed_event_decoder_handles_split_records() {
        let event = sample_events()[0];
        let mut encoded = [0_u8; PACKED_EVENT_RECORD_BYTES];
        encode_packed_event(event, &mut encoded);

        let mut decoder = PackedEventPreviewDecoder::default();
        let mut out = Vec::new();
        decoder
            .decode_bytes(&encoded[..5], &mut out)
            .expect("first chunk must decode");
        assert!(out.is_empty());
        decoder
            .decode_bytes(&encoded[5..], &mut out)
            .expect("second chunk must decode");
        assert_eq!(out, vec![event]);
    }

    #[test]
    fn open_csv_replay_and_seek_from_shared_events() {
        let path = temp_path("csv");
        let events = sample_events();
        write_csv(&path, 640, 480, &events);

        let (mut camera, controls, info, shared_events) =
            DecodedEventFileCamera::open(&path).expect("csv replay must open");
        assert_eq!(info.width, 640);
        assert_eq!(info.height, 480);
        assert_eq!(
            info.file_size,
            (events.len() * PACKED_EVENT_RECORD_BYTES) as u64
        );
        camera.start_streaming().expect("stream must start");

        let mut buf = [0_u8; PACKED_EVENT_RECORD_BYTES * 2];
        let n = camera
            .read_packet(&mut buf)
            .expect("first read must succeed");
        assert_eq!(n, PACKED_EVENT_RECORD_BYTES * 2);
        assert_eq!(
            controls.bytes_read.load(Ordering::Relaxed),
            (PACKED_EVENT_RECORD_BYTES * 2) as u64
        );

        let (mut seeked, seek_controls) =
            DecodedEventFileCamera::open_at(shared_events, &info, PACKED_EVENT_RECORD_BYTES as u64)
                .expect("seek reopen must succeed");
        seeked.start_streaming().expect("seeked stream must start");
        let mut seek_buf = [0_u8; PACKED_EVENT_RECORD_BYTES * 4];
        let seek_n = seeked
            .read_packet(&mut seek_buf)
            .expect("seeked read must succeed");
        assert_eq!(seek_n, PACKED_EVENT_RECORD_BYTES * 2);
        assert_eq!(
            seek_controls.bytes_read.load(Ordering::Relaxed),
            (PACKED_EVENT_RECORD_BYTES * 3) as u64
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn decoded_replay_event_source_fetches_timestamp_ranges() {
        let source = DecodedReplayEventSource::new(Arc::new(sample_events()));

        let chunk = source.fetch_range(110, 150).expect("range fetch succeeds");

        assert_eq!(
            chunk.events,
            vec![
                CompactEvent::from(CdEvent {
                    x: 11,
                    y: 21,
                    polarity: false,
                    timestamp: 120,
                }),
                CompactEvent::from(CdEvent {
                    x: 12,
                    y: 22,
                    polarity: true,
                    timestamp: 150,
                }),
            ]
        );
        assert!(matches!(
            source.fetch_range(151, 200),
            Err(FetchError::OutOfTimeline)
        ));
    }

    #[test]
    fn open_bin_replay_reads_header_and_events() {
        let path = temp_path("bin");
        let events = sample_events();
        write_bin(&path, 320, 240, &events);

        let (_camera, _controls, info, shared_events) =
            DecodedEventFileCamera::open(&path).expect("bin replay must open");
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert_eq!(&*shared_events, &events);

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn open_npy_replay_infers_minimum_geometry() {
        let path = temp_path("npy");
        let events = sample_events();
        write_npy(&path, &events);

        let (_camera, _controls, info, shared_events) =
            DecodedEventFileCamera::open(&path).expect("npy replay must open");
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 720);
        assert_eq!(&*shared_events, &events);

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn speed_epoch_reset_restarts_decoded_replay_baseline() {
        let path = temp_path("csv");
        let events = sample_events();
        write_csv(&path, 640, 480, &events);

        let (mut camera, controls, _info, _shared_events) =
            DecodedEventFileCamera::open(&path).expect("csv replay must open");
        camera.event_idx = 2;
        camera.session_start_ts_us = events[0].timestamp;
        camera.playback_started_at = Some(Instant::now() - Duration::from_secs(2));
        camera.paused_at = Some(Instant::now());
        camera.total_paused = Duration::from_millis(250);

        controls.speed_epoch.fetch_add(1, Ordering::Relaxed);
        camera
            .throttle_to_current_progress()
            .expect("speed-change throttle reset must succeed");

        assert_eq!(camera.session_start_ts_us, events[2].timestamp);
        assert_eq!(camera.last_speed_epoch, 1);
        assert_eq!(camera.total_paused, Duration::ZERO);
        assert!(camera.paused_at.is_none());
        assert!(
            camera
                .playback_started_at
                .expect("baseline must be reset")
                .elapsed()
                < Duration::from_secs(1)
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[cfg(feature = "hdf5")]
    #[test]
    fn open_hdf5_replay_reads_geometry_and_events() {
        let path = temp_path("h5");
        write_hdf5(
            &path,
            Some("320x240"),
            &[
                RawHdf5CdEvent {
                    x: 10,
                    y: 20,
                    p: 1,
                    t: 100,
                },
                RawHdf5CdEvent {
                    x: 11,
                    y: 21,
                    p: 0,
                    t: 120,
                },
            ],
        );

        let (_camera, _controls, info, shared_events) =
            DecodedEventFileCamera::open(&path).expect("hdf5 replay must open");
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert_eq!(
            &*shared_events,
            &[
                CdEvent {
                    x: 10,
                    y: 20,
                    polarity: true,
                    timestamp: 100,
                },
                CdEvent {
                    x: 11,
                    y: 21,
                    polarity: false,
                    timestamp: 120,
                },
            ]
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[cfg(feature = "hdf5")]
    #[test]
    fn open_hdf5_replay_reads_ascii_geometry_and_events() {
        let path = temp_path("h5");
        write_hdf5_ascii(
            &path,
            Some("320x240"),
            &[
                RawHdf5CdEvent {
                    x: 10,
                    y: 20,
                    p: 1,
                    t: 100,
                },
                RawHdf5CdEvent {
                    x: 11,
                    y: 21,
                    p: 0,
                    t: 120,
                },
            ],
        );

        let (_camera, _controls, info, shared_events) =
            DecodedEventFileCamera::open(&path).expect("hdf5 replay must open");
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert_eq!(
            &*shared_events,
            &[
                CdEvent {
                    x: 10,
                    y: 20,
                    polarity: true,
                    timestamp: 100,
                },
                CdEvent {
                    x: 11,
                    y: 21,
                    polarity: false,
                    timestamp: 120,
                },
            ]
        );

        fs::remove_file(path).expect("temp file must be removed");
    }
}
