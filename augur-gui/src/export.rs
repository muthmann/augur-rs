use std::{
    fs::File,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use augur_core::{
    config::RoiConfig,
    pipeline::{CdEvent, Evt3CorePreviewDecoder, PreviewDecoder, BUF_SIZE},
};
use tiff::encoder::{colortype::Gray16, TiffEncoder};

#[derive(Debug, Clone)]
pub(crate) struct TiffStackExportParams {
    pub(crate) acq_time_us: u64,
    pub(crate) start_us: u64,
    pub(crate) end_us: u64,
    pub(crate) roi: Option<RoiConfig>,
    pub(crate) output_path: PathBuf,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

#[derive(Debug, Clone)]
pub(crate) enum ExportEventSource {
    Decoded(Arc<Vec<CdEvent>>),
    RawEvt3 { path: PathBuf, data_offset: u64 },
}

pub(crate) fn estimate_tiff_stack_frames(start_us: u64, end_us: u64, acq_time_us: u64) -> usize {
    if end_us <= start_us || acq_time_us == 0 {
        return 0;
    }

    let duration_us = end_us - start_us;
    duration_us
        .saturating_add(acq_time_us - 1)
        .saturating_div(acq_time_us)
        .min(usize::MAX as u64) as usize
}

pub(crate) fn export_tiff_stack(
    source: ExportEventSource,
    params: &TiffStackExportParams,
) -> Result<usize, String> {
    let validated = ValidatedExportParams::new(params)?;
    let file = File::create(&validated.output_path)
        .map_err(|err| format!("creating {} failed: {err}", validated.output_path.display()))?;
    let writer = BufWriter::new(file);
    let mut exporter = TiffStackWriter::new(writer, validated)?;

    match source {
        ExportEventSource::Decoded(events) => {
            for event in events.iter().copied() {
                if !exporter.process_event(event)? {
                    break;
                }
            }
        }
        ExportEventSource::RawEvt3 { path, data_offset } => {
            export_raw_evt3_events(&path, data_offset, &mut exporter)?;
        }
    }

    exporter.finish()
}

#[derive(Debug, Clone)]
struct ValidatedExportParams {
    acq_time_us: u64,
    start_us: u64,
    end_us: u64,
    roi: Option<RoiConfig>,
    output_path: PathBuf,
    width: u16,
    height: u16,
    output_width: u32,
    output_height: u32,
}

impl ValidatedExportParams {
    fn new(params: &TiffStackExportParams) -> Result<Self, String> {
        if params.acq_time_us == 0 {
            return Err("acquisition time must be > 0".into());
        }
        if params.width == 0 || params.height == 0 {
            return Err("export dimensions must be > 0".into());
        }
        if params.end_us <= params.start_us {
            return Err("export end time must be greater than start time".into());
        }

        let roi = match params.roi {
            Some(roi) => {
                roi.validate(params.width, params.height)
                    .map_err(|err| format!("invalid export ROI: {err}"))?;
                Some(roi)
            }
            None => None,
        };
        let (output_width, output_height) = roi
            .map(|roi| (u32::from(roi.width), u32::from(roi.height)))
            .unwrap_or((u32::from(params.width), u32::from(params.height)));

        Ok(Self {
            acq_time_us: params.acq_time_us,
            start_us: params.start_us,
            end_us: params.end_us,
            roi,
            output_path: params.output_path.clone(),
            width: params.width,
            height: params.height,
            output_width,
            output_height,
        })
    }
}

struct TiffStackWriter<W> {
    encoder: TiffEncoder<W>,
    params: ValidatedExportParams,
    frame_start_us: u64,
    frame_end_us: u64,
    full_frame: Vec<u16>,
    roi_frame: Vec<u16>,
    frame_count: usize,
    finished: bool,
}

impl<W> TiffStackWriter<W>
where
    W: Write + Seek,
{
    fn new(writer: W, params: ValidatedExportParams) -> Result<Self, String> {
        let encoder = TiffEncoder::new(writer)
            .map_err(|err| format!("creating TIFF encoder failed: {err}"))?;
        let pixel_count = usize::from(params.width) * usize::from(params.height);
        Ok(Self {
            encoder,
            frame_start_us: params.start_us,
            frame_end_us: params.start_us.saturating_add(params.acq_time_us),
            full_frame: vec![0; pixel_count],
            roi_frame: Vec::new(),
            frame_count: 0,
            finished: false,
            params,
        })
    }

    fn process_event(&mut self, event: CdEvent) -> Result<bool, String> {
        if self.finished {
            return Ok(false);
        }
        if event.timestamp < self.params.start_us {
            return Ok(true);
        }

        while event.timestamp >= self.frame_end_us && self.frame_start_us < self.params.end_us {
            self.write_current_frame()?;
            self.advance_frame();
        }

        if self.frame_start_us >= self.params.end_us || event.timestamp >= self.params.end_us {
            self.finished = true;
            return Ok(false);
        }

        if event.x < self.params.width && event.y < self.params.height {
            let idx = usize::from(event.y) * usize::from(self.params.width) + usize::from(event.x);
            self.full_frame[idx] = self.full_frame[idx].saturating_add(1);
        }

        Ok(true)
    }

    fn finish(mut self) -> Result<usize, String> {
        while self.frame_start_us < self.params.end_us {
            self.write_current_frame()?;
            self.advance_frame();
        }

        Ok(self.frame_count)
    }

    fn write_current_frame(&mut self) -> Result<(), String> {
        let image = self
            .encoder
            .new_image::<Gray16>(self.params.output_width, self.params.output_height)
            .map_err(|err| format!("creating TIFF frame {} failed: {err}", self.frame_count + 1))?;

        if let Some(roi) = self.params.roi {
            self.roi_frame.clear();
            self.roi_frame
                .reserve_exact(usize::from(roi.width) * usize::from(roi.height));
            for y in roi.y..roi.y.saturating_add(roi.height) {
                let start = usize::from(y) * usize::from(self.params.width) + usize::from(roi.x);
                let end = start + usize::from(roi.width);
                self.roi_frame
                    .extend_from_slice(&self.full_frame[start..end]);
            }
            image.write_data(&self.roi_frame).map_err(|err| {
                format!("writing TIFF frame {} failed: {err}", self.frame_count + 1)
            })?;
        } else {
            image.write_data(&self.full_frame).map_err(|err| {
                format!("writing TIFF frame {} failed: {err}", self.frame_count + 1)
            })?;
        }

        self.frame_count += 1;
        self.full_frame.fill(0);
        self.roi_frame.clear();
        Ok(())
    }

    fn advance_frame(&mut self) {
        self.frame_start_us = self.frame_end_us;
        self.frame_end_us = self.frame_end_us.saturating_add(self.params.acq_time_us);
    }
}

fn export_raw_evt3_events<W>(
    path: &Path,
    data_offset: u64,
    exporter: &mut TiffStackWriter<W>,
) -> Result<(), String>
where
    W: Write + Seek,
{
    let mut file =
        File::open(path).map_err(|err| format!("opening {} failed: {err}", path.display()))?;
    file.seek(SeekFrom::Start(data_offset))
        .map_err(|err| format!("seeking {} failed: {err}", path.display()))?;

    let mut decoder = Evt3CorePreviewDecoder::default();
    let mut read_buf = vec![0_u8; BUF_SIZE];
    let mut events = Vec::new();
    loop {
        let bytes_read = file
            .read(&mut read_buf)
            .map_err(|err| format!("reading {} failed: {err}", path.display()))?;
        if bytes_read == 0 {
            break;
        }

        decoder
            .decode_bytes(&read_buf[..bytes_read], &mut events)
            .map_err(|err| format!("decoding {} failed: {err}", path.display()))?;
        for event in events.iter().copied() {
            if !exporter.process_event(event)? {
                return Ok(());
            }
        }
    }

    decoder
        .finish_stream()
        .map_err(|err| format!("finalizing {} decode failed: {err}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        io::BufReader,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use tiff::decoder::{Decoder, DecodingResult};

    use super::{
        estimate_tiff_stack_frames, export_tiff_stack, ExportEventSource, TiffStackExportParams,
    };
    use augur_core::{config::RoiConfig, pipeline::CdEvent};

    #[test]
    fn estimated_frame_count_rounds_up_partial_windows() {
        assert_eq!(estimate_tiff_stack_frames(0, 100_000, 50_000), 2);
        assert_eq!(estimate_tiff_stack_frames(0, 100_001, 50_000), 3);
        assert_eq!(estimate_tiff_stack_frames(50_000, 50_000, 50_000), 0);
    }

    #[test]
    fn decoded_export_writes_multi_page_roi_tiff() {
        let output_path = unique_temp_path("decoded_roi_stack");
        let events = Arc::new(vec![
            CdEvent {
                x: 2,
                y: 2,
                timestamp: 20_000,
                polarity: true,
            },
            CdEvent {
                x: 1,
                y: 1,
                timestamp: 60_000,
                polarity: false,
            },
        ]);
        let params = TiffStackExportParams {
            acq_time_us: 50_000,
            start_us: 0,
            end_us: 100_000,
            roi: Some(RoiConfig {
                x: 1,
                y: 1,
                width: 2,
                height: 2,
            }),
            output_path: output_path.clone(),
            width: 4,
            height: 3,
        };

        let frame_count = export_tiff_stack(ExportEventSource::Decoded(events), &params).unwrap();
        assert_eq!(frame_count, 2);

        let mut decoder = Decoder::new(BufReader::new(File::open(&output_path).unwrap())).unwrap();
        let mut pages = Vec::new();
        loop {
            assert_eq!(decoder.dimensions().unwrap(), (2, 2));
            match decoder.read_image().unwrap() {
                DecodingResult::U16(pixels) => pages.push(pixels),
                other => panic!("unexpected TIFF pixel format: {other:?}"),
            }
            if decoder.more_images() {
                decoder.next_image().unwrap();
            } else {
                break;
            }
        }

        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0], vec![0, 0, 0, 1]);
        assert_eq!(pages[1], vec![1, 0, 0, 0]);

        fs::remove_file(output_path).unwrap();
    }

    fn unique_temp_path(stem: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{stem}_{}_{}.tiff", std::process::id(), nanos))
    }
}
