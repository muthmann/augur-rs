use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use evt3_core::{CdEvent as Evt3CdEvent, Evt3Decoder, TriggerEvent as Evt3TriggerEvent};

use crate::{
    camera::{DeviceInfo, EventCamera, PacketStreamCamera},
    config::CameraConfig,
    metadata::RecordingMetadata,
    CameraError, Result,
};

const HEADER_END: &str = "% end";
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const THROTTLE_SLEEP_SLICE: Duration = Duration::from_millis(10);
const FAST_SCAN_WINDOW_BYTES: u64 = 128 * 1024;
/// EVT3 timestamps are 24 bits wide (TIME_HIGH:12 | TIME_LOW:12), so they
/// wrap around every 2^24 µs ≈ 16.8 seconds.
const EVT3_TIMESTAMP_PERIOD_US: u64 = 1 << 24;

#[derive(Debug, Clone)]
pub struct ReplayFileInfo {
    pub file_size: u64,
    pub data_offset: u64,
    pub width: u16,
    pub height: u16,
    pub metadata: RecordingMetadata,
    pub total_duration_us: u64,
    pub first_timestamp_us: u64,
    pub nominal_bytes_per_sec: Option<f64>,
}

impl ReplayFileInfo {
    pub fn data_len(&self) -> u64 {
        self.file_size.saturating_sub(self.data_offset)
    }
}

#[derive(Debug, Clone)]
pub struct ReplayControls {
    pub paused: Arc<AtomicBool>,
    pub speed_bits: Arc<AtomicU32>,
    pub speed_epoch: Arc<AtomicU32>,
    pub bytes_read: Arc<AtomicU64>,
    pub current_timestamp_us: Arc<AtomicU64>,
    pub file_size: u64,
    pub data_offset: u64,
    pub width: u16,
    pub height: u16,
    pub total_duration_us: u64,
    pub first_timestamp_us: u64,
}

impl ReplayControls {
    pub(crate) fn new(info: &ReplayFileInfo, bytes_read: u64) -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            speed_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            speed_epoch: Arc::new(AtomicU32::new(0)),
            bytes_read: Arc::new(AtomicU64::new(bytes_read)),
            current_timestamp_us: Arc::new(AtomicU64::new(0)),
            file_size: info.file_size,
            data_offset: info.data_offset,
            width: info.width,
            height: info.height,
            total_duration_us: info.total_duration_us,
            first_timestamp_us: info.first_timestamp_us,
        }
    }
}

#[derive(Debug)]
pub struct RawFileCamera {
    file: File,
    info: DeviceInfo,
    controls: ReplayControls,
    bytes_read: u64,
    session_start_ts_us: u64,
    timing_decoder: Evt3Decoder,
    timing_cd_scratch: Vec<Evt3CdEvent>,
    timing_trigger_scratch: Vec<Evt3TriggerEvent>,
    last_speed_epoch: u32,
    playback_started_at: Option<Instant>,
    paused_at: Option<Instant>,
    total_paused: Duration,
}

impl RawFileCamera {
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, ReplayControls, ReplayFileInfo)> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let (data_offset, width, height, metadata) = parse_evt3_header(&file)?;
        if data_offset > file_size {
            return Err(CameraError::Config(format!(
                "raw file header offset {data_offset} exceeds file size {file_size}"
            )));
        }

        let data_len = file_size.saturating_sub(data_offset);
        let (first_timestamp_us, last_timestamp_us, nominal_bytes_per_sec) =
            scan_duration_fast(&file, data_offset, data_len)?;
        let total_duration_us = last_timestamp_us
            .zip(first_timestamp_us)
            .and_then(|(last, first)| last.checked_sub(first))
            .unwrap_or(0);

        let info = ReplayFileInfo {
            file_size,
            data_offset,
            width,
            height,
            metadata,
            total_duration_us,
            first_timestamp_us: first_timestamp_us.unwrap_or(0),
            nominal_bytes_per_sec,
        };
        let (camera, controls) = Self::open_at(path, &info, info.data_offset)?;
        Ok((camera, controls, info))
    }

    pub fn open_at(
        path: impl AsRef<Path>,
        info: &ReplayFileInfo,
        start_byte: u64,
    ) -> Result<(Self, ReplayControls)> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let aligned_start = align_evt3_word_offset(info.data_offset, start_byte)
            .clamp(info.data_offset, info.file_size);
        let start_data_bytes = aligned_start.saturating_sub(info.data_offset);
        file.seek(SeekFrom::Start(aligned_start))?;

        let controls = ReplayControls::new(info, start_data_bytes);
        let device_info = build_device_info(path, &info.metadata);

        Ok((
            Self {
                file,
                info: device_info,
                controls: controls.clone(),
                bytes_read: start_data_bytes,
                session_start_ts_us: 0,
                timing_decoder: Evt3Decoder::default(),
                timing_cd_scratch: Vec::with_capacity(4_096),
                timing_trigger_scratch: Vec::with_capacity(256),
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

    fn reset_timing_baseline(&mut self, now: Instant, current_ts: u64) {
        self.session_start_ts_us = current_ts;
        self.playback_started_at = Some(now);
        self.total_paused = Duration::ZERO;
        self.paused_at = None;
    }

    fn reset_throttle_baseline_if_needed(&mut self, now: Instant, current_ts: u64) {
        let epoch = self.controls.speed_epoch.load(Ordering::Relaxed);
        if epoch == self.last_speed_epoch {
            return;
        }

        self.last_speed_epoch = epoch;
        self.reset_timing_baseline(now, current_ts);
    }

    fn throttle_to_current_progress(&mut self) -> Result<()> {
        let speed = self.speed_multiplier();
        if !speed.is_finite() || speed <= 0.0 {
            return Ok(());
        }

        let now = Instant::now();
        let current_ts = self.controls.current_timestamp_us.load(Ordering::Relaxed);
        self.reset_throttle_baseline_if_needed(now, current_ts);

        if current_ts == 0 {
            return Ok(());
        }
        if self.session_start_ts_us == 0 {
            self.reset_timing_baseline(now, current_ts);
        }

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

    fn update_current_timestamp_from_packet(&mut self, bytes: &[u8]) -> Result<()> {
        self.timing_cd_scratch.clear();
        self.timing_trigger_scratch.clear();
        self.timing_decoder
            .decode_bytes(
                bytes,
                &mut self.timing_cd_scratch,
                &mut self.timing_trigger_scratch,
            )
            .map_err(|e| CameraError::Other(format!("raw replay timing decode failed: {e}")))?;

        if let Some(last) = self.timing_cd_scratch.last() {
            self.controls
                .current_timestamp_us
                .fetch_max(last.timestamp, Ordering::Relaxed);
        }

        Ok(())
    }
}

impl EventCamera for RawFileCamera {
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

impl PacketStreamCamera for RawFileCamera {
    fn read_packet(&mut self, buf: &mut [u8]) -> Result<usize> {
        let now = Instant::now();
        if self.controls.paused.load(Ordering::Relaxed) {
            self.mark_paused(now);
            thread::sleep(PAUSE_POLL_INTERVAL);
            return Err(CameraError::Timeout("replay paused".into()));
        }

        self.mark_resumed(now);
        self.throttle_to_current_progress()?;

        let n = self.file.read(buf)?;
        if n == 0 {
            return Err(CameraError::Eof);
        }

        self.update_current_timestamp_from_packet(&buf[..n])?;
        self.bytes_read += n as u64;
        self.controls
            .bytes_read
            .store(self.bytes_read, Ordering::Relaxed);
        Ok(n)
    }
}

fn build_device_info(path: &Path, metadata: &RecordingMetadata) -> DeviceInfo {
    let fallback_model = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    DeviceInfo {
        vendor: metadata
            .system_id
            .clone()
            .unwrap_or_else(|| "AugurRS".into()),
        model: metadata.system_id.clone().unwrap_or(fallback_model),
        serial: metadata.serial_number.clone(),
        firmware: metadata.firmware_version.clone(),
        compatible: metadata.sensor_compatible.clone(),
    }
}

fn parse_evt3_header(file: &File) -> Result<(u64, u16, u16, RecordingMetadata)> {
    let mut reader = BufReader::new(file.try_clone()?);
    let mut buf = Vec::new();
    let mut geometry = None;
    let mut format_geometry = None;
    let mut metadata_pairs = Vec::new();
    // If a % format line is present it must declare EVT3; if absent we assume EVT3.
    let mut format_rejected = false;

    loop {
        buf.clear();
        let bytes = reader.read_until(b'\n', &mut buf)?;
        if bytes == 0 {
            return Err(CameraError::Config(
                "raw file is missing the EVT3 header terminator".into(),
            ));
        }

        // Older files may have non-UTF-8 bytes in the header; use lossy conversion.
        let line = String::from_utf8_lossy(&buf);
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("% format ") {
            match parse_format_line(rest) {
                Ok(geom) => format_geometry = Some(geom),
                Err(_) => format_rejected = true,
            }
        } else if let Some(rest) = trimmed.strip_prefix("% geometry ") {
            geometry = Some(parse_geometry_line(rest)?);
        } else if trimmed.starts_with("% evt ") {
            continue;
        } else if trimmed == HEADER_END {
            let data_offset = reader.stream_position()?;
            if format_rejected {
                return Err(CameraError::Config(
                    "raw file declares a non-EVT3 format".into(),
                ));
            }
            let (width, height) = geometry
                .or(format_geometry)
                .ok_or_else(|| CameraError::Config("raw file header is missing geometry".into()))?;
            return Ok((
                data_offset,
                width,
                height,
                RecordingMetadata::from_header_lines(metadata_pairs),
            ));
        } else if let Some((key, value)) = parse_metadata_header_line(trimmed) {
            metadata_pairs.push((key, value));
        }
    }
}

fn parse_metadata_header_line(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("% ")?;
    let split_at = rest.find(char::is_whitespace)?;
    let key = rest[..split_at].trim();
    let value = rest[split_at..].trim();
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key.to_owned(), value.to_owned()))
}

fn parse_format_line(rest: &str) -> Result<(u16, u16)> {
    let mut parts = rest.split(';');
    let format = parts.next().unwrap_or_default().trim();
    if format != "EVT3" {
        return Err(CameraError::Config(format!(
            "unsupported raw file format {format:?}; expected EVT3"
        )));
    }

    let mut width = None;
    let mut height = None;
    for part in parts {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        match key.trim() {
            "width" => width = Some(parse_u16(value, "width")?),
            "height" => height = Some(parse_u16(value, "height")?),
            _ => {}
        }
    }

    let width = width.ok_or_else(|| CameraError::Config("format line missing width".into()))?;
    let height = height.ok_or_else(|| CameraError::Config("format line missing height".into()))?;
    Ok((width, height))
}

fn parse_geometry_line(rest: &str) -> Result<(u16, u16)> {
    let (width, height) = rest
        .trim()
        .split_once('x')
        .ok_or_else(|| CameraError::Config("invalid geometry line".into()))?;
    Ok((parse_u16(width, "width")?, parse_u16(height, "height")?))
}

fn parse_u16(raw: &str, label: &str) -> Result<u16> {
    raw.trim().parse::<u16>().map_err(|e| {
        CameraError::Config(format!(
            "failed to parse {label} value {:?}: {e}",
            raw.trim()
        ))
    })
}

fn scan_duration_fast(
    file: &File,
    data_offset: u64,
    data_len: u64,
) -> Result<(Option<u64>, Option<u64>, Option<f64>)> {
    if data_len < 2 {
        return Ok((None, None, None));
    }

    let window_len = data_len.min(FAST_SCAN_WINDOW_BYTES);
    let first_start = data_offset;
    let last_start =
        data_offset + align_relative_evt3_word_offset(data_len.saturating_sub(window_len));
    // Capture both timestamps from the first window so we can compute a local
    // bytes-per-µs rate without crossing window boundaries.
    let (first_ts, first_window_last_ts) = scan_timestamp_window(file, first_start, window_len)?;
    let (_, last_ts_raw) = scan_timestamp_window(file, last_start, window_len)?;

    // Each scan window uses a fresh Evt3Decoder that has no knowledge of the
    // recording history, so it decodes TIME_HIGH values modulo 4096.  The
    // EVT3 24-bit timestamp wraps every 2^24 µs ≈ 16.8 s.  For recordings
    // longer than one period the last window's `last_ts_raw` is smaller than
    // the true absolute timestamp.  We estimate the total duration from the
    // first window's internal rate and pick the rollover-corrected value of
    // `last_ts_raw` that is closest to that estimate.
    let first_window_duration_us = first_ts
        .zip(first_window_last_ts)
        .and_then(|(first, last)| last.checked_sub(first))
        .filter(|&d| d > 0);

    let last_ts: Option<u64> = match (first_ts, last_ts_raw, first_window_duration_us) {
        (Some(first), Some(last_raw), Some(window_dur_us)) => {
            let est_total_us =
                (data_len as f64 / window_len as f64 * window_dur_us as f64).round() as u64;
            Some(correct_evt3_rollover(first, last_raw, est_total_us))
        }
        (_, raw, _) => raw,
    };

    let nominal_bytes_per_sec = first_ts
        .zip(last_ts)
        .and_then(|(first, last)| last.checked_sub(first))
        .filter(|&d| d > 0)
        .map(|d| data_len as f64 / (d as f64 / 1_000_000.0));

    Ok((first_ts, last_ts, nominal_bytes_per_sec))
}

/// Corrects `last_ts_raw` for EVT3 24-bit timestamp rollover.
///
/// `last_ts_raw` comes from a fresh decoder that decoded only the tail of the
/// recording, so it does not account for how many full 2^24 µs periods elapsed
/// since the beginning.  Given an estimate of the total recording duration we
/// pick the nearest rollover-adjusted candidate.
fn correct_evt3_rollover(first_ts: u64, last_ts_raw: u64, est_total_us: u64) -> u64 {
    let expected_last = first_ts.saturating_add(est_total_us);
    let period = EVT3_TIMESTAMP_PERIOD_US;
    let n = expected_last / period;
    let candidates = [
        n.saturating_sub(1)
            .saturating_mul(period)
            .saturating_add(last_ts_raw),
        n.saturating_mul(period).saturating_add(last_ts_raw),
        n.saturating_add(1)
            .saturating_mul(period)
            .saturating_add(last_ts_raw),
    ];
    candidates
        .into_iter()
        .min_by_key(|&c| c.abs_diff(expected_last))
        .unwrap_or(last_ts_raw)
}

fn scan_timestamp_window(file: &File, start: u64, len: u64) -> Result<(Option<u64>, Option<u64>)> {
    let aligned_len = align_relative_evt3_word_offset(len);
    if aligned_len < 2 {
        return Ok((None, None));
    }

    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(start))?;

    let mut bytes = vec![0_u8; aligned_len as usize];
    clone.read_exact(&mut bytes)?;

    let mut decoder = Evt3Decoder::default();
    let mut cd_events = Vec::<Evt3CdEvent>::with_capacity(4_096);
    let mut trigger_events = Vec::<Evt3TriggerEvent>::with_capacity(256);
    decoder
        .decode_bytes(&bytes, &mut cd_events, &mut trigger_events)
        .map_err(|e| CameraError::Other(format!("failed to scan replay timestamps: {e}")))?;
    decoder.finish_stream_lenient();

    Ok((
        cd_events.first().map(|event| event.timestamp),
        cd_events.last().map(|event| event.timestamp),
    ))
}

fn align_evt3_word_offset(data_offset: u64, absolute_offset: u64) -> u64 {
    data_offset + align_relative_evt3_word_offset(absolute_offset.saturating_sub(data_offset))
}

pub fn align_relative_evt3_word_offset(relative_offset: u64) -> u64 {
    relative_offset & !1
}

fn lift_evt3_timestamp_near(raw_timestamp_us: u64, expected_timestamp_us: u64) -> u64 {
    let cycle = expected_timestamp_us / EVT3_TIMESTAMP_PERIOD_US;
    [
        cycle
            .saturating_sub(1)
            .saturating_mul(EVT3_TIMESTAMP_PERIOD_US)
            .saturating_add(raw_timestamp_us),
        cycle
            .saturating_mul(EVT3_TIMESTAMP_PERIOD_US)
            .saturating_add(raw_timestamp_us),
        cycle
            .saturating_add(1)
            .saturating_mul(EVT3_TIMESTAMP_PERIOD_US)
            .saturating_add(raw_timestamp_us),
    ]
    .into_iter()
    .min_by_key(|candidate| candidate.abs_diff(expected_timestamp_us))
    .unwrap_or(raw_timestamp_us)
}

fn scan_timestamp_window_near(
    file: &File,
    start: u64,
    len: u64,
    expected_timestamp_us: u64,
) -> Result<Option<(u64, u64)>> {
    let (Some(first_raw), Some(last_raw)) = scan_timestamp_window(file, start, len)? else {
        return Ok(None);
    };

    let first_abs = lift_evt3_timestamp_near(first_raw, expected_timestamp_us);
    let mut last_abs = lift_evt3_timestamp_near(last_raw, expected_timestamp_us);
    while last_abs < first_abs {
        last_abs = last_abs.saturating_add(EVT3_TIMESTAMP_PERIOD_US);
    }
    Ok(Some((first_abs, last_abs)))
}

fn replay_timestamp_interval_distance(
    target_timestamp_us: u64,
    first_us: u64,
    last_us: u64,
) -> u64 {
    if target_timestamp_us < first_us {
        first_us - target_timestamp_us
    } else {
        target_timestamp_us.saturating_sub(last_us)
    }
}

fn clamp_raw_search_start(info: &ReplayFileInfo, start: u64, window_len: u64) -> u64 {
    let max_start = info
        .file_size
        .saturating_sub(window_len)
        .max(info.data_offset);
    align_evt3_word_offset(info.data_offset, start.clamp(info.data_offset, max_start))
}

pub fn raw_replay_offset_for_timestamp(
    path: impl AsRef<Path>,
    info: &ReplayFileInfo,
    target_timestamp_us: u64,
    desired_window_us: u64,
) -> Result<u64> {
    let data_len = info.data_len();
    if data_len < 2 {
        return Ok(info.data_offset);
    }

    let path = path.as_ref();
    let file = File::open(path)?;
    let recording_end_ts = info
        .first_timestamp_us
        .saturating_add(info.total_duration_us);
    let target_timestamp_us = target_timestamp_us.clamp(info.first_timestamp_us, recording_end_ts);

    let mut window_len =
        align_relative_evt3_word_offset(data_len.min(FAST_SCAN_WINDOW_BYTES)).max(2);
    let desired_window_bytes = info
        .nominal_bytes_per_sec
        .map(|bytes_per_sec| {
            ((bytes_per_sec * desired_window_us.max(1) as f64) / 1_000_000.0).round() as u64
        })
        .unwrap_or(window_len)
        .min(window_len)
        .max(window_len.min(4_096));
    let min_window_len = align_relative_evt3_word_offset(desired_window_bytes)
        .max(2)
        .min(window_len);

    let target_rel = target_timestamp_us.saturating_sub(info.first_timestamp_us);
    let guess_rel = if info.total_duration_us == 0 {
        0
    } else {
        ((data_len as u128 * target_rel as u128) / info.total_duration_us as u128) as u64
    };
    let mut best_start = clamp_raw_search_start(info, info.data_offset + guess_rel, window_len);

    let coarse_steps = 8_u64;
    for step in 0..=coarse_steps {
        let coarse_rel = if coarse_steps == 0 {
            0
        } else {
            data_len.saturating_mul(step) / coarse_steps
        };
        let candidate_start =
            clamp_raw_search_start(info, info.data_offset + coarse_rel, window_len);
        let score = match scan_timestamp_window_near(
            &file,
            candidate_start,
            window_len,
            target_timestamp_us,
        )? {
            Some((first_us, last_us)) => (
                replay_timestamp_interval_distance(target_timestamp_us, first_us, last_us),
                first_us
                    .abs_diff(target_timestamp_us)
                    .min(last_us.abs_diff(target_timestamp_us)),
                candidate_start.abs_diff(best_start),
            ),
            None => (u64::MAX, u64::MAX, u64::MAX),
        };
        let best_score =
            match scan_timestamp_window_near(&file, best_start, window_len, target_timestamp_us)? {
                Some((first_us, last_us)) => (
                    replay_timestamp_interval_distance(target_timestamp_us, first_us, last_us),
                    first_us
                        .abs_diff(target_timestamp_us)
                        .min(last_us.abs_diff(target_timestamp_us)),
                    0,
                ),
                None => (u64::MAX, u64::MAX, u64::MAX),
            };
        if score < best_score {
            best_start = candidate_start;
        }
    }

    loop {
        let mut best_candidate = best_start;
        let mut best_score =
            match scan_timestamp_window_near(&file, best_start, window_len, target_timestamp_us)? {
                Some((first_us, last_us)) => (
                    replay_timestamp_interval_distance(target_timestamp_us, first_us, last_us),
                    first_us
                        .abs_diff(target_timestamp_us)
                        .min(last_us.abs_diff(target_timestamp_us)),
                    0,
                ),
                None => (u64::MAX, u64::MAX, u64::MAX),
            };

        for candidate_start in [
            best_start.saturating_sub(window_len),
            best_start,
            best_start.saturating_add(window_len),
        ] {
            let candidate_start = clamp_raw_search_start(info, candidate_start, window_len);
            let score = match scan_timestamp_window_near(
                &file,
                candidate_start,
                window_len,
                target_timestamp_us,
            )? {
                Some((first_us, last_us)) => (
                    replay_timestamp_interval_distance(target_timestamp_us, first_us, last_us),
                    first_us
                        .abs_diff(target_timestamp_us)
                        .min(last_us.abs_diff(target_timestamp_us)),
                    candidate_start.abs_diff(best_start),
                ),
                None => (u64::MAX, u64::MAX, u64::MAX),
            };
            if score < best_score {
                best_score = score;
                best_candidate = candidate_start;
            }
        }

        best_start = best_candidate;
        if window_len <= min_window_len {
            return Ok(best_start);
        }

        let next_window_len = align_relative_evt3_word_offset(window_len / 2).max(2);
        if next_window_len == window_len {
            return Ok(best_start);
        }
        window_len = next_window_len.max(min_window_len);
        best_start = clamp_raw_search_start(info, best_start, window_len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    // ── correct_evt3_rollover ────────────────────────────────────────────────

    #[test]
    fn rollover_correction_no_rollover() {
        // Short recording that never wraps: correction must be a no-op.
        let first_ts = 1_000_u64;
        let true_last = 5_000_000_u64; // 5 s
        let corrected = correct_evt3_rollover(first_ts, true_last, true_last - first_ts);
        assert_eq!(corrected, true_last);
    }

    #[test]
    fn rollover_correction_one_rollover_25s() {
        // A 25-second recording.  EVT3 period = 2^24 = 16,777,216 µs.
        // first_ts = 1,000; true last_ts = 25,001,000.
        // After one rollover: last_ts_raw = 25,001,000 - 16,777,216 = 8,223,784.
        let first_ts = 1_000_u64;
        let true_last = 25_001_000_u64;
        let last_ts_raw = true_last - EVT3_TIMESTAMP_PERIOD_US; // 8,223,784
        let est_total_us = true_last - first_ts; // 25,000,000
        let corrected = correct_evt3_rollover(first_ts, last_ts_raw, est_total_us);
        assert_eq!(corrected, true_last);
    }

    #[test]
    fn rollover_correction_two_rollovers_40s() {
        // A 40-second recording crosses the period boundary twice.
        let first_ts = 500_u64;
        let true_last = 40_000_500_u64;
        let last_ts_raw = true_last - 2 * EVT3_TIMESTAMP_PERIOD_US;
        let est_total_us = true_last - first_ts;
        let corrected = correct_evt3_rollover(first_ts, last_ts_raw, est_total_us);
        assert_eq!(corrected, true_last);
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock must be after epoch")
            .as_nanos();
        env::temp_dir().join(format!("augur-replay-{name}-{nanos}.raw"))
    }

    fn sample_raw_bytes() -> Vec<u8> {
        let words: [u16; 8] = [
            (0x8 << 12) | 0x001,
            (0x6 << 12) | 0x010,
            7,
            (0x2 << 12) | 100,
            (0x8 << 12) | 0x001,
            (0x6 << 12) | 0x020,
            7,
            (0x2 << 12) | 101,
        ];
        let mut bytes = Vec::with_capacity(words.len() * 2);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    fn write_sample_raw(path: &Path) -> (u64, Vec<u8>) {
        let body = sample_raw_bytes();
        let header = concat!(
            "% format EVT3;width=1280;height=720\n",
            "% geometry 1280x720\n",
            "% evt 3.0\n",
            "% serial_number 00a1b2c3d4e5f678\n",
            "% system_id Prophesee EVK4\n",
            "% firmware_version 0x040200\n",
            "% sensor_compatible imx636,ccam5_gen42\n",
            "% augur_version 0.2.0\n",
            "% recording_date 2026-04-01T14:30:00Z\n",
            "% recording_hostname lab-workstation-03\n",
            "% pixel_pitch_nm 4860\n",
            "% custom_field retained\n",
            "% end\n"
        );
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        fs::write(path, bytes).expect("sample raw file must be written");
        (header.len() as u64, body)
    }

    fn raw_event_words(timestamp: u64, x: u16) -> [u16; 4] {
        let time_high = ((timestamp >> 12) & 0x0fff) as u16;
        let time_low = (timestamp & 0x0fff) as u16;
        [
            (0x8 << 12) | time_high,
            (0x6 << 12) | time_low,
            7,
            (0x2 << 12) | x,
        ]
    }

    fn write_raw_with_timestamps(path: &Path, timestamps: &[u64]) -> u64 {
        let header = concat!(
            "% format EVT3;width=1280;height=720\n",
            "% geometry 1280x720\n",
            "% evt 3.0\n",
            "% end\n"
        );
        let mut bytes = header.as_bytes().to_vec();
        for (idx, timestamp) in timestamps.iter().copied().enumerate() {
            for word in raw_event_words(timestamp, 100 + idx as u16) {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
        }
        fs::write(path, bytes).expect("timestamped raw sample must be written");
        header.len() as u64
    }

    #[test]
    fn open_parses_evt3_header_and_fast_scan_metadata() {
        let path = temp_path("header");
        let (data_offset, _body) = write_sample_raw(&path);

        let (_camera, controls, info) = RawFileCamera::open(&path).expect("raw file must open");

        assert_eq!(controls.width, 1280);
        assert_eq!(controls.height, 720);
        assert_eq!(controls.data_offset, data_offset);
        assert_eq!(controls.first_timestamp_us, 0x1010);
        assert_eq!(controls.total_duration_us, 0x10);
        assert!(controls.file_size > data_offset);
        assert_eq!(info.data_offset, data_offset);
        assert_eq!(info.first_timestamp_us, 0x1010);
        assert_eq!(info.total_duration_us, 0x10);
        assert!(info.nominal_bytes_per_sec.is_some());
        assert_eq!(
            info.metadata.serial_number.as_deref(),
            Some("00a1b2c3d4e5f678")
        );
        assert_eq!(info.metadata.system_id.as_deref(), Some("Prophesee EVK4"));
        assert_eq!(info.metadata.pixel_pitch_nm, Some(4_860.0));
        assert_eq!(
            info.metadata.extra.get("custom_field").map(String::as_str),
            Some("retained")
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn open_at_starts_from_requested_offset_and_updates_absolute_progress() {
        let path = temp_path("seek");
        let (data_offset, body) = write_sample_raw(&path);
        let (_camera, _controls, info) = RawFileCamera::open(&path).expect("raw file must open");
        let start_byte = data_offset + 4;

        let (mut camera, controls) =
            RawFileCamera::open_at(&path, &info, start_byte).expect("seeked open must work");
        camera.start_streaming().expect("start must succeed");

        assert_eq!(controls.bytes_read.load(Ordering::Relaxed), 4);
        let mut buf = [0_u8; 256];
        let n = camera
            .read_packet(&mut buf)
            .expect("read after seek must succeed");
        assert_eq!(controls.bytes_read.load(Ordering::Relaxed), 4 + n as u64);
        assert_eq!(&buf[..n], &body[4..]);

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn raw_replay_device_info_uses_header_metadata() {
        let path = temp_path("device-info");
        write_sample_raw(&path);

        let (camera, _controls, _info) = RawFileCamera::open(&path).expect("raw file must open");
        let device_info = camera.device_info();

        assert_eq!(device_info.model, "Prophesee EVK4");
        assert_eq!(device_info.serial.as_deref(), Some("00a1b2c3d4e5f678"));
        assert_eq!(device_info.firmware.as_deref(), Some("0x040200"));
        assert_eq!(
            device_info.compatible.as_deref(),
            Some("imx636,ccam5_gen42")
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn read_packet_updates_current_timestamp_feedback() {
        let path = temp_path("timestamp-feedback");
        write_sample_raw(&path);

        let (mut camera, controls, _info) = RawFileCamera::open(&path).expect("raw file must open");
        camera.start_streaming().expect("start must succeed");

        let mut buf = [0_u8; 8];
        camera
            .read_packet(&mut buf)
            .expect("first packet read must succeed");
        assert_eq!(
            controls.current_timestamp_us.load(Ordering::Relaxed),
            0x1010
        );

        camera
            .read_packet(&mut buf)
            .expect("second packet read must succeed");
        assert_eq!(
            controls.current_timestamp_us.load(Ordering::Relaxed),
            0x1020
        );

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn read_packet_reports_eof() {
        let path = temp_path("eof");
        write_sample_raw(&path);

        let (mut camera, _controls, _info) =
            RawFileCamera::open(&path).expect("raw file must open");
        camera.start_streaming().expect("start must succeed");

        let mut buf = [0_u8; 256];
        camera
            .read_packet(&mut buf)
            .expect("first read must succeed");
        let err = camera
            .read_packet(&mut buf)
            .expect_err("second read must reach EOF");
        assert!(matches!(err, CameraError::Eof));

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn paused_read_returns_timeout() {
        let path = temp_path("pause");
        write_sample_raw(&path);

        let (mut camera, controls, _info) = RawFileCamera::open(&path).expect("raw file must open");
        controls.paused.store(true, Ordering::Relaxed);

        let err = camera
            .read_packet(&mut [0_u8; 16])
            .expect_err("paused replay must time out");
        assert!(matches!(err, CameraError::Timeout(_)));

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn speed_epoch_reset_restarts_throttle_baseline() {
        let path = temp_path("speed");
        write_sample_raw(&path);

        let (mut camera, controls, _info) = RawFileCamera::open(&path).expect("raw file must open");
        controls
            .current_timestamp_us
            .store(24_000, Ordering::Relaxed);
        camera.session_start_ts_us = 2_000;
        camera.playback_started_at = Some(Instant::now() - Duration::from_secs(2));
        camera.paused_at = Some(Instant::now());
        camera.total_paused = Duration::from_millis(250);

        controls.speed_epoch.fetch_add(1, Ordering::Relaxed);
        camera
            .throttle_to_current_progress()
            .expect("speed-change throttle reset must succeed");

        assert_eq!(camera.session_start_ts_us, 24_000);
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

    #[test]
    fn raw_time_seek_offset_beats_naive_time_fraction_in_dense_region() {
        let path = temp_path("timestamp-target");
        let mut timestamps: Vec<u64> = (0..700).map(|idx| 0x1100 + idx as u64).collect();
        timestamps.extend((0..8).map(|idx| 0x5000 + idx as u64 * 0x10));
        let data_offset = write_raw_with_timestamps(&path, &timestamps);
        let (_camera, _controls, info) = RawFileCamera::open(&path).expect("raw file must open");
        let target_timestamp_us = 0x1200_u64;
        let target_rel = target_timestamp_us.saturating_sub(info.first_timestamp_us);
        let naive_offset = data_offset
            + align_relative_evt3_word_offset(
                ((info.data_len() as u128 * target_rel as u128) / info.total_duration_us as u128)
                    as u64,
            );
        let refined_offset =
            raw_replay_offset_for_timestamp(&path, &info, target_timestamp_us, 128)
                .expect("timestamp-targeted raw seek must work");

        let (mut naive_camera, naive_controls) =
            RawFileCamera::open_at(&path, &info, naive_offset).expect("naive seek must open");
        naive_camera
            .start_streaming()
            .expect("naive seek must start");
        naive_camera
            .read_packet(&mut [0_u8; 8])
            .expect("naive seek read must succeed");

        let (mut refined_camera, refined_controls) =
            RawFileCamera::open_at(&path, &info, refined_offset).expect("refined seek must open");
        refined_camera
            .start_streaming()
            .expect("refined seek must start");
        refined_camera
            .read_packet(&mut [0_u8; 8])
            .expect("refined seek read must succeed");

        let naive_delta = naive_controls
            .current_timestamp_us
            .load(Ordering::Relaxed)
            .abs_diff(target_timestamp_us);
        let refined_delta = refined_controls
            .current_timestamp_us
            .load(Ordering::Relaxed)
            .abs_diff(target_timestamp_us);

        assert!(
            refined_delta < naive_delta,
            "expected refined raw seek ({refined_delta}) to beat naive byte-fraction seek ({naive_delta})"
        );
        assert!(
            refined_delta <= 0x80,
            "expected refined raw seek delta {refined_delta} to stay within one frame window"
        );

        fs::remove_file(path).expect("temp file must be removed");
    }
}
