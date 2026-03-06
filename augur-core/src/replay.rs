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
const TIMESTAMP_SCAN_BUF_SIZE: usize = 65_536;

#[derive(Debug, Clone)]
pub struct ReplayControls {
    pub paused: Arc<AtomicBool>,
    pub speed_bits: Arc<AtomicU32>,
    pub bytes_read: Arc<AtomicU64>,
    pub file_size: u64,
    pub data_offset: u64,
    pub width: u16,
    pub height: u16,
}

impl ReplayControls {
    fn new(file_size: u64, data_offset: u64, width: u16, height: u16) -> Self {
        Self {
            paused: Arc::new(AtomicBool::new(false)),
            speed_bits: Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            bytes_read: Arc::new(AtomicU64::new(0)),
            file_size,
            data_offset,
            width,
            height,
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
    playback_started_at: Option<Instant>,
    paused_at: Option<Instant>,
    total_paused: Duration,
}

impl RawFileCamera {
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, ReplayControls)> {
        let path = path.as_ref();
        let mut file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let (data_offset, width, height) = parse_evt3_header(&file)?;
        if data_offset > file_size {
            return Err(CameraError::Config(format!(
                "raw file header offset {data_offset} exceeds file size {file_size}"
            )));
        }
        let data_len = file_size.saturating_sub(data_offset);
        let nominal_bytes_per_sec = estimate_nominal_bytes_per_sec(&file, data_offset, data_len)?;
        file.seek(SeekFrom::Start(data_offset))?;

        let controls = ReplayControls::new(file_size, data_offset, width, height);
        let info = DeviceInfo {
            vendor: "AugurRS".into(),
            model: "RAW File Replay".into(),
            serial: None,
            firmware: None,
            compatible: Some(
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string()),
            ),
        };

        Ok((
            Self {
                file,
                info,
                controls: controls.clone(),
                nominal_bytes_per_sec,
                bytes_read: 0,
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
        if !speed.is_finite() || speed <= 0.0 || self.bytes_read == 0 {
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
            let desired_elapsed_s = self.bytes_read as f64 / (base_rate * speed as f64);
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

fn parse_evt3_header(file: &File) -> Result<(u64, u16, u16)> {
    let mut reader = BufReader::new(file.try_clone()?);
    let mut line = String::new();
    let mut geometry = None;
    let mut format_geometry = None;
    let mut saw_format = false;

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Err(CameraError::Config(
                "raw file is missing the EVT3 header terminator".into(),
            ));
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some(rest) = trimmed.strip_prefix("% format ") {
            saw_format = true;
            format_geometry = Some(parse_format_line(rest)?);
        } else if let Some(rest) = trimmed.strip_prefix("% geometry ") {
            geometry = Some(parse_geometry_line(rest)?);
        } else if trimmed == HEADER_END {
            let data_offset = reader.stream_position()?;
            let (width, height) = geometry
                .or(format_geometry)
                .ok_or_else(|| CameraError::Config("raw file header is missing geometry".into()))?;
            if !saw_format {
                return Err(CameraError::Config(
                    "raw file header is missing the EVT3 format line".into(),
                ));
            }
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

fn estimate_nominal_bytes_per_sec(
    file: &File,
    data_offset: u64,
    data_len: u64,
) -> Result<Option<f64>> {
    if data_len == 0 {
        return Ok(None);
    }

    let mut clone = file.try_clone()?;
    clone.seek(SeekFrom::Start(data_offset))?;

    let mut decoder = Evt3Decoder::default();
    let mut cd_events = Vec::<Evt3CdEvent>::with_capacity(4_096);
    let mut trigger_events = Vec::<Evt3TriggerEvent>::with_capacity(256);
    let mut buf = [0_u8; TIMESTAMP_SCAN_BUF_SIZE];
    let mut first_ts = None;
    let mut last_ts = None;

    loop {
        let n = clone.read(&mut buf)?;
        if n == 0 {
            break;
        }

        cd_events.clear();
        trigger_events.clear();
        decoder
            .decode_bytes(&buf[..n], &mut cd_events, &mut trigger_events)
            .map_err(|e| CameraError::Other(format!("failed to scan replay timestamps: {e}")))?;

        if let Some(event) = cd_events.first() {
            first_ts.get_or_insert(event.timestamp);
        }
        if let Some(event) = cd_events.last() {
            last_ts = Some(event.timestamp);
        }
    }

    decoder.finish_stream().map_err(|e| {
        CameraError::Other(format!("failed to finalize replay timestamp scan: {e}"))
    })?;

    let Some((first_ts, last_ts)) = first_ts.zip(last_ts) else {
        return Ok(None);
    };
    if last_ts <= first_ts {
        return Ok(None);
    }

    let duration_s = (last_ts - first_ts) as f64 / 1_000_000.0;
    if duration_s <= 0.0 {
        return Ok(None);
    }

    Ok(Some(data_len as f64 / duration_s))
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

    fn write_sample_raw(path: &Path) -> u64 {
        let body = sample_raw_bytes();
        let header = "% format EVT3;width=1280;height=720\n% geometry 1280x720\n% evt 3.0\n% end\n";
        let mut bytes = header.as_bytes().to_vec();
        bytes.extend_from_slice(&body);
        fs::write(path, bytes).expect("sample raw file must be written");
        header.len() as u64
    }

    #[test]
    fn open_parses_evt3_header_and_geometry() {
        let path = temp_path("header");
        let data_offset = write_sample_raw(&path);

        let (_camera, controls) = RawFileCamera::open(&path).expect("raw file must open");

        assert_eq!(controls.width, 1280);
        assert_eq!(controls.height, 720);
        assert_eq!(controls.data_offset, data_offset);
        assert!(controls.file_size > data_offset);

        fs::remove_file(path).expect("temp file must be removed");
    }

    #[test]
    fn read_packet_updates_progress_and_reports_eof() {
        let path = temp_path("eof");
        write_sample_raw(&path);

        let (mut camera, controls) = RawFileCamera::open(&path).expect("raw file must open");
        camera.start_streaming().expect("start must succeed");

        let mut buf = [0_u8; 256];
        let n = camera
            .read_packet(&mut buf)
            .expect("first read must succeed");
        assert!(n > 0);
        assert_eq!(controls.bytes_read.load(Ordering::Relaxed), n as u64);

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

        let (mut camera, controls) = RawFileCamera::open(&path).expect("raw file must open");
        controls.paused.store(true, Ordering::Relaxed);

        let err = camera
            .read_packet(&mut [0_u8; 16])
            .expect_err("paused replay must time out");
        assert!(matches!(err, CameraError::Timeout(_)));

        fs::remove_file(path).expect("temp file must be removed");
    }
}
