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
    CameraError, Result,
};

const HEADER_END: &str = "% end";
const PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const THROTTLE_SLEEP_SLICE: Duration = Duration::from_millis(10);
const FAST_SCAN_WINDOW_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone)]
pub struct ReplayFileInfo {
    pub file_size: u64,
    pub data_offset: u64,
    pub width: u16,
    pub height: u16,
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
    pub bytes_read: Arc<AtomicU64>,
    pub file_size: u64,
    pub data_offset: u64,
    pub width: u16,
    pub height: u16,
    pub total_duration_us: u64,
    pub first_timestamp_us: u64,
}

impl ReplayControls {
    fn new(info: &ReplayFileInfo, bytes_read: u64) -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            speed_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            bytes_read: Arc::new(AtomicU64::new(bytes_read)),
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
    nominal_bytes_per_sec: Option<f64>,
    bytes_read: u64,
    session_start_bytes: u64,
    playback_started_at: Option<Instant>,
    paused_at: Option<Instant>,
    total_paused: Duration,
}

impl RawFileCamera {
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, ReplayControls, ReplayFileInfo)> {
        let path = path.as_ref();
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let (data_offset, width, height) = parse_evt3_header(&file)?;
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
        let device_info = build_device_info(path);

        Ok((
            Self {
                file,
                info: device_info,
                controls: controls.clone(),
                nominal_bytes_per_sec: info.nominal_bytes_per_sec,
                bytes_read: start_data_bytes,
                session_start_bytes: start_data_bytes,
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

    fn throttle_to_current_progress(&mut self) -> Result<()> {
        let Some(base_rate) = self.nominal_bytes_per_sec else {
            return Ok(());
        };
        let speed = self.speed_multiplier();
        let session_bytes = self.bytes_read.saturating_sub(self.session_start_bytes);
        if !speed.is_finite() || speed <= 0.0 || session_bytes == 0 {
            return Ok(());
        }

        let started_at = *self.playback_started_at.get_or_insert_with(Instant::now);
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
            let desired_elapsed_s = session_bytes as f64 / (base_rate * speed as f64);
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

        self.bytes_read += n as u64;
        self.controls
            .bytes_read
            .store(self.bytes_read, Ordering::Relaxed);
        Ok(n)
    }
}

fn build_device_info(path: &Path) -> DeviceInfo {
    DeviceInfo {
        vendor: "AugurRS".into(),
        model: "RAW File Replay".into(),
        serial: None,
        firmware: None,
        compatible: Some(
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
        ),
    }
}

fn parse_evt3_header(file: &File) -> Result<(u64, u16, u16)> {
    let mut reader = BufReader::new(file.try_clone()?);
    let mut buf = Vec::new();
    let mut geometry = None;
    let mut format_geometry = None;
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
            return Ok((data_offset, width, height));
        }
    }
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
    let (first_ts, _) = scan_timestamp_window(file, first_start, window_len)?;
    let (_, last_ts) = scan_timestamp_window(file, last_start, window_len)?;

    let nominal_bytes_per_sec = first_ts
        .zip(last_ts)
        .and_then(|(first, last)| last.checked_sub(first))
        .filter(|duration_us| *duration_us > 0)
        .map(|duration_us| data_len as f64 / (duration_us as f64 / 1_000_000.0));

    Ok((first_ts, last_ts, nominal_bytes_per_sec))
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
    // finish_stream may fail for older files whose EVT3 stream has no clean terminator;
    // this is non-fatal — we already have whatever timestamps were decoded.
    let _ = decoder.finish_stream();

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

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        env, fs,
        time::{SystemTime, UNIX_EPOCH},
    };

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
        let header = "% format EVT3;width=1280;height=720\n% geometry 1280x720\n% evt 3.0\n% end\n";
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        fs::write(path, bytes).expect("sample raw file must be written");
        (header.len() as u64, body)
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
}
