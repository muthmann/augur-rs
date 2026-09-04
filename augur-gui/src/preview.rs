use std::cell::RefCell;

use augur_core::pipeline::{CdEvent, PreviewFrame, PREVIEW_HISTOGRAM_BINS};
use egui::{Color32, ColorImage};

use crate::{colormap::Colormap, theme};

thread_local! {
    static PREVIEW_SCRATCH: RefCell<PreviewRenderScratch> = RefCell::new(PreviewRenderScratch::default());
}

const MAX_HISTOGRAM_BINS: usize = PREVIEW_HISTOGRAM_BINS;
pub(crate) const TIME_SURFACE_BINS: usize = 256;
pub(crate) const TIME_SURFACE_TICK_US: u64 = 64;

#[derive(Default)]
struct PreviewRenderScratch {
    time_surface_ticks: Vec<u32>,
    time_surface_size: [usize; 2],
    time_surface_frame_end_us: Option<u64>,
    time_surface_values: Vec<u8>,
    time_surface_decay_key: Option<TimeSurfaceDecayKey>,
    time_surface_histogram: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimeSurfaceDecayKey {
    frame_end_us: u64,
    tau_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PreviewDisplaySettings {
    pub display_min: u16,
    pub display_max: u16,
    pub gamma: f32,
}

impl Default for PreviewDisplaySettings {
    fn default() -> Self {
        Self {
            display_min: 0,
            display_max: 255,
            gamma: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisplayLutKey {
    display_min: u16,
    display_max: u16,
    gamma_bits: u32,
}

impl From<PreviewDisplaySettings> for DisplayLutKey {
    fn from(settings: PreviewDisplaySettings) -> Self {
        let display_min = settings
            .display_min
            .min(settings.display_max.saturating_sub(1));
        let display_max = settings.display_max.max(display_min.saturating_add(1));
        Self {
            display_min,
            display_max,
            gamma_bits: settings.gamma.max(0.01).to_bits(),
        }
    }
}

impl DisplayLutKey {
    fn brightness_u8(self, value: u16) -> u8 {
        let range = self.display_max.saturating_sub(self.display_min).max(1) as f32;
        let normalized =
            (f32::from(value.saturating_sub(self.display_min)) / range).clamp(0.0, 1.0);
        (normalized.powf(f32::from_bits(self.gamma_bits)) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewMode {
    RedBlue,
    SignedCount,
    Intensity(Colormap),
    TimeSurface,
}

impl Default for PreviewMode {
    fn default() -> Self {
        Self::Intensity(Colormap::Grays)
    }
}

impl PreviewMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::RedBlue => "Polarity",
            Self::SignedCount => "Signed Count",
            Self::Intensity(colormap) => colormap.label(),
            Self::TimeSurface => "Time Surface",
        }
    }

    pub fn requires_raw_events(self) -> bool {
        matches!(self, Self::TimeSurface)
    }

    pub fn ramp_label(self) -> &'static str {
        match self {
            Self::RedBlue => "Polarity (magenta/cyan)",
            Self::SignedCount => "Signed-count diverging ramp",
            Self::Intensity(_) => "Intensity display ramp",
            Self::TimeSurface => "Time-surface decay ramp",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum PreparedPreviewFrame<'a> {
    IntensityR16 {
        size: [usize; 2],
        values: &'a [u16],
    },
    PolarityRg16 {
        size: [usize; 2],
        total: &'a [u16],
        on: &'a [u16],
        off: &'a [u16],
    },
    TimeSurfaceR8 {
        size: [usize; 2],
        values: &'a [u8],
    },
}

impl PreparedPreviewFrame<'_> {
    pub fn size(self) -> [usize; 2] {
        match self {
            Self::IntensityR16 { size, .. }
            | Self::PolarityRg16 { size, .. }
            | Self::TimeSurfaceR8 { size, .. } => size,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct CpuPreviewImageCache {
    lut_key: Option<DisplayLutKey>,
    brightness_lut: Vec<u8>,
    image: ColorImage,
}

impl CpuPreviewImageCache {
    pub fn render_prepared(
        &mut self,
        prepared: PreparedPreviewFrame<'_>,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
    ) -> &ColorImage {
        self.ensure_brightness_lut(settings);
        self.ensure_image(prepared.size());
        match prepared {
            PreparedPreviewFrame::IntensityR16 { values, .. } => {
                let table = match mode {
                    PreviewMode::Intensity(colormap) => colormap.table(),
                    _ => Colormap::Grays.table(),
                };
                for (pixel, &value) in self.image.pixels.iter_mut().zip(values) {
                    *pixel = table[self.brightness_lut[value as usize] as usize];
                }
            }
            PreparedPreviewFrame::PolarityRg16 { total, on, off, .. } => match mode {
                PreviewMode::RedBlue => {
                    for (((pixel, &total), &on), &off) in
                        self.image.pixels.iter_mut().zip(total).zip(on).zip(off)
                    {
                        *pixel = if total == 0 {
                            Color32::BLACK
                        } else {
                            let brightness = self.brightness_lut[total as usize];
                            match on.cmp(&off) {
                                std::cmp::Ordering::Greater => {
                                    polarity_color(brightness, theme::POLARITY_ON_RGB)
                                }
                                std::cmp::Ordering::Less => {
                                    polarity_color(brightness, theme::POLARITY_OFF_RGB)
                                }
                                std::cmp::Ordering::Equal => polarity_balanced_color(brightness),
                            }
                        };
                    }
                }
                PreviewMode::SignedCount => {
                    let diverging = Colormap::BlueWhiteRed.table();
                    for (((pixel, &total), &on), &off) in
                        self.image.pixels.iter_mut().zip(total).zip(on).zip(off)
                    {
                        *pixel = if total == 0 {
                            Color32::BLACK
                        } else {
                            let magnitude =
                                f32::from(self.brightness_lut[on.abs_diff(off) as usize]) / 255.0;
                            let signed_t = match on.cmp(&off) {
                                std::cmp::Ordering::Greater => 0.5 + 0.5 * magnitude,
                                std::cmp::Ordering::Less => 0.5 - 0.5 * magnitude,
                                std::cmp::Ordering::Equal => 0.5,
                            };
                            diverging[(signed_t * 255.0).round().clamp(0.0, 255.0) as usize]
                        };
                    }
                }
                _ => unreachable!("polarity payload is only used for polarity preview modes"),
            },
            PreparedPreviewFrame::TimeSurfaceR8 { values, .. } => {
                let table = Colormap::Grays.table();
                for (pixel, &value) in self.image.pixels.iter_mut().zip(values) {
                    *pixel = table[self.brightness_lut[usize::from(value)] as usize];
                }
            }
        }

        &self.image
    }

    fn ensure_brightness_lut(&mut self, settings: PreviewDisplaySettings) {
        let key = DisplayLutKey::from(settings);
        if self.lut_key == Some(key) && self.brightness_lut.len() == usize::from(u16::MAX) + 1 {
            return;
        }

        self.brightness_lut.resize(usize::from(u16::MAX) + 1, 0);
        for (value, entry) in self.brightness_lut.iter_mut().enumerate() {
            *entry = key.brightness_u8(value as u16);
        }
        self.lut_key = Some(key);
    }

    fn ensure_image(&mut self, size: [usize; 2]) {
        if self.image.size != size || self.image.pixels.len() != size[0].saturating_mul(size[1]) {
            self.image = ColorImage::filled(size, Color32::BLACK);
        }
    }
}

/// Modulate a fully-saturated polarity tint by a brightness in `[0, 255]`.
fn polarity_color(brightness: u8, tint: [u8; 3]) -> Color32 {
    let scale = brightness as f32 / 255.0;
    let scale_channel = |c: u8| ((c as f32) * scale).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(
        scale_channel(tint[0]),
        scale_channel(tint[1]),
        scale_channel(tint[2]),
    )
}

/// Pixel where the ON / OFF counts are exactly equal — average the two
/// polarity tints so the colour is unmistakably between the two endpoints.
fn polarity_balanced_color(brightness: u8) -> Color32 {
    let on = theme::POLARITY_ON_RGB;
    let off = theme::POLARITY_OFF_RGB;
    let tint = [
        ((on[0] as u16 + off[0] as u16) / 2) as u8,
        ((on[1] as u16 + off[1] as u16) / 2) as u8,
        ((on[2] as u16 + off[2] as u16) / 2) as u8,
    ];
    polarity_color(brightness, tint)
}

pub fn reset_preview_render_cache() {
    PREVIEW_SCRATCH.with(|scratch| {
        *scratch.borrow_mut() = PreviewRenderScratch::default();
    });
}

pub(crate) fn query_time_surface_value(
    frame: &PreviewFrame,
    time_surface_tau_us: u64,
    index: usize,
) -> Option<u8> {
    PREVIEW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        if !ensure_time_surface_state(frame, &mut scratch) {
            return None;
        }

        let key = TimeSurfaceDecayKey {
            frame_end_us: frame.window_end_us,
            tau_us: time_surface_tau_us.max(1),
        };
        if scratch.time_surface_decay_key == Some(key) {
            return scratch.time_surface_values.get(index).copied();
        }

        let tick = *scratch.time_surface_ticks.get(index)?;
        Some(time_surface_value_u8_from_tick(
            tick,
            encode_time_surface_tick(key.frame_end_us),
            key.tau_us,
        ))
    })
}

pub fn with_prepared_preview_frame<R>(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    f: impl for<'a> FnOnce(PreparedPreviewFrame<'a>) -> R,
) -> R {
    let size = [frame.width as usize, frame.height as usize];
    match mode {
        PreviewMode::Intensity(_) => f(PreparedPreviewFrame::IntensityR16 {
            size,
            values: &frame.pixels,
        }),
        PreviewMode::RedBlue | PreviewMode::SignedCount => f(PreparedPreviewFrame::PolarityRg16 {
            size,
            total: &frame.pixels,
            on: &frame.pixels_on,
            off: &frame.pixels_off,
        }),
        PreviewMode::TimeSurface => PREVIEW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            if ensure_time_surface_render_cache(frame, time_surface_tau_us, &mut scratch) {
                f(PreparedPreviewFrame::TimeSurfaceR8 {
                    size,
                    values: &scratch.time_surface_values,
                })
            } else {
                f(PreparedPreviewFrame::IntensityR16 {
                    size,
                    values: &frame.pixels,
                })
            }
        }),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn frame_to_color_image(
    frame: &PreviewFrame,
    settings: PreviewDisplaySettings,
    mode: PreviewMode,
    time_surface_tau_us: u64,
) -> ColorImage {
    let mut cache = CpuPreviewImageCache::default();
    with_prepared_preview_frame(frame, mode, time_surface_tau_us, |prepared| {
        cache.render_prepared(prepared, settings, mode).clone()
    })
}

pub fn compute_frame_histogram(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
) -> Vec<u64> {
    with_frame_histogram(frame, mode, time_surface_tau_us, |histogram| {
        trimmed_histogram(histogram).to_vec()
    })
}

pub fn compute_auto_contrast_max(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    percentile: f32,
) -> u16 {
    with_frame_histogram(frame, mode, time_surface_tau_us, |histogram| {
        percentile_bin(histogram, percentile)
    })
}

pub fn cached_frame_histogram(frame: &PreviewFrame, mode: PreviewMode) -> Option<&[u64]> {
    let histogram = match mode {
        PreviewMode::Intensity(_) | PreviewMode::RedBlue => &frame.cached_total_histogram,
        PreviewMode::SignedCount => &frame.cached_signed_histogram,
        PreviewMode::TimeSurface => return None,
    };
    (!histogram.is_empty()).then_some(histogram.as_slice())
}

fn compute_prepared_histogram(prepared: PreparedPreviewFrame<'_>, mode: PreviewMode) -> Vec<u64> {
    match prepared {
        PreparedPreviewFrame::IntensityR16 { values, .. } => {
            histogram_from_values(values.iter().copied())
        }
        PreparedPreviewFrame::PolarityRg16 { total, on, off, .. } => match mode {
            PreviewMode::SignedCount => {
                histogram_from_values(on.iter().zip(off).map(|(&on, &off)| on.abs_diff(off)))
            }
            _ => histogram_from_values(total.iter().copied()),
        },
        PreparedPreviewFrame::TimeSurfaceR8 { values, .. } => {
            histogram_from_values(values.iter().copied().map(u16::from))
        }
    }
}

fn with_frame_histogram<R>(
    frame: &PreviewFrame,
    mode: PreviewMode,
    time_surface_tau_us: u64,
    f: impl FnOnce(&[u64]) -> R,
) -> R {
    if let Some(histogram) = cached_frame_histogram(frame, mode) {
        return f(histogram);
    }

    if matches!(mode, PreviewMode::TimeSurface) {
        return PREVIEW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            if ensure_time_surface_render_cache(frame, time_surface_tau_us, &mut scratch) {
                f(&scratch.time_surface_histogram)
            } else {
                let histogram = histogram_from_values(frame.pixels.iter().copied());
                f(&histogram)
            }
        });
    }

    with_prepared_preview_frame(frame, mode, time_surface_tau_us, |prepared| {
        let histogram = compute_prepared_histogram(prepared, mode);
        f(&histogram)
    })
}

fn ensure_time_surface_state(frame: &PreviewFrame, scratch: &mut PreviewRenderScratch) -> bool {
    let size = [frame.width as usize, frame.height as usize];
    let pixel_count = size[0].saturating_mul(size[1]);
    let geometry_changed =
        scratch.time_surface_size != size || scratch.time_surface_ticks.len() != pixel_count;
    let timestamp_regressed = scratch
        .time_surface_frame_end_us
        .is_some_and(|last_end| frame.window_end_us < last_end);

    if geometry_changed || timestamp_regressed {
        scratch.time_surface_ticks.resize(pixel_count, 0);
        scratch.time_surface_ticks.fill(0);
        scratch.time_surface_size = size;
        scratch.time_surface_frame_end_us = None;
        scratch.time_surface_values.resize(pixel_count, 0);
        scratch.time_surface_values.fill(0);
        scratch.time_surface_decay_key = None;
        scratch.time_surface_histogram.resize(TIME_SURFACE_BINS, 0);
        scratch.time_surface_histogram.fill(0);
    }

    if scratch.time_surface_frame_end_us == Some(frame.window_end_us) {
        return true;
    }

    let Some(events) = frame.events_snapshot() else {
        return false;
    };
    update_time_surface(
        &events,
        frame.width,
        frame.height,
        &mut scratch.time_surface_ticks,
    );
    scratch.time_surface_frame_end_us = Some(frame.window_end_us);
    scratch.time_surface_decay_key = None;
    true
}

fn ensure_time_surface_render_cache(
    frame: &PreviewFrame,
    time_surface_tau_us: u64,
    scratch: &mut PreviewRenderScratch,
) -> bool {
    if !ensure_time_surface_state(frame, scratch) {
        return false;
    }

    let key = TimeSurfaceDecayKey {
        frame_end_us: frame.window_end_us,
        tau_us: time_surface_tau_us.max(1),
    };
    let pixel_count = frame.width as usize * frame.height as usize;
    if scratch.time_surface_decay_key == Some(key)
        && scratch.time_surface_values.len() == pixel_count
    {
        return true;
    }

    scratch.time_surface_values.resize(pixel_count, 0);
    scratch.time_surface_histogram.resize(TIME_SURFACE_BINS, 0);
    scratch.time_surface_histogram.fill(0);
    let reference_tick = encode_time_surface_tick(frame.window_end_us);
    for (index, &tick) in scratch.time_surface_ticks.iter().enumerate() {
        let value = time_surface_value_u8_from_tick(tick, reference_tick, key.tau_us);
        scratch.time_surface_values[index] = value;
        scratch.time_surface_histogram[usize::from(value)] += 1;
    }
    scratch.time_surface_decay_key = Some(key);
    true
}

fn update_time_surface(events: &[CdEvent], width: u16, height: u16, time_surface: &mut [u32]) {
    for event in events {
        if event.x >= width || event.y >= height {
            continue;
        }
        let index = event.y as usize * width as usize + event.x as usize;
        let encoded_tick = encode_time_surface_tick(event.timestamp);
        time_surface[index] = time_surface[index].max(encoded_tick);
    }
}

pub(crate) fn encode_time_surface_tick(timestamp_us: u64) -> u32 {
    let tick = (timestamp_us / TIME_SURFACE_TICK_US).min(u64::from(u32::MAX - 1));
    tick as u32 + 1
}

pub(crate) fn time_surface_value_u8_from_tick(tick: u32, reference_tick: u32, tau_us: u64) -> u8 {
    if tick == 0 {
        return 0;
    }

    let dt_ticks = u64::from(reference_tick.saturating_sub(tick));
    let dt = dt_ticks.saturating_mul(TIME_SURFACE_TICK_US);
    let tau_us = tau_us.max(1) as f64;
    let value = (-(dt as f64) / tau_us).exp();
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn histogram_from_values(values: impl IntoIterator<Item = u16>) -> Vec<u64> {
    let mut histogram = vec![0_u64; MAX_HISTOGRAM_BINS];
    let mut max_bin = 0_usize;
    let mut saw_value = false;
    for value in values {
        let index = (value as usize).min(MAX_HISTOGRAM_BINS - 1);
        histogram[index] += 1;
        max_bin = max_bin.max(index);
        saw_value = true;
    }
    histogram.truncate(if saw_value { max_bin + 1 } else { 1 });
    histogram
}

fn trimmed_histogram(histogram: &[u64]) -> &[u64] {
    let len = histogram
        .iter()
        .rposition(|&count| count != 0)
        .map(|index| index + 1)
        .unwrap_or(1);
    &histogram[..len]
}

fn percentile_bin(histogram: &[u64], percentile: f32) -> u16 {
    if histogram.is_empty() {
        return 1;
    }

    let total: u64 = histogram.iter().sum();
    if total == 0 {
        return 1;
    }

    let target =
        ((total as f64 * percentile.clamp(0.0, 100.0) as f64 / 100.0).ceil() as u64).max(1);
    let mut cumulative = 0_u64;
    for (bin, count) in histogram.iter().copied().enumerate() {
        cumulative = cumulative.saturating_add(count);
        if cumulative >= target {
            return bin.min(u16::MAX as usize) as u16;
        }
    }
    histogram.len().saturating_sub(1).min(u16::MAX as usize) as u16
}

#[cfg(test)]
mod tests {
    use super::{
        compute_frame_histogram, frame_to_color_image, query_time_surface_value,
        reset_preview_render_cache, with_prepared_preview_frame, PreparedPreviewFrame,
        PreviewDisplaySettings, PreviewMode, MAX_HISTOGRAM_BINS, TIME_SURFACE_BINS,
    };
    use crate::colormap::Colormap;
    use augur_core::pipeline::{CdEvent, PreviewFrame};

    fn cached_histogram(values: &[u16]) -> Vec<u64> {
        let mut histogram = vec![0_u64; MAX_HISTOGRAM_BINS];
        for &value in values {
            let index = usize::from(value).min(MAX_HISTOGRAM_BINS - 1);
            histogram[index] += 1;
        }
        histogram
    }

    fn cached_signed_histogram(on: &[u16], off: &[u16]) -> Vec<u64> {
        let mut histogram = vec![0_u64; MAX_HISTOGRAM_BINS];
        for (&on, &off) in on.iter().zip(off) {
            let index = usize::from(on.abs_diff(off)).min(MAX_HISTOGRAM_BINS - 1);
            histogram[index] += 1;
        }
        histogram
    }

    #[test]
    fn histogram_counts_combined_pixels() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![0, 2, 2],
            pixels_on: vec![0, 1, 2],
            pixels_off: vec![0, 1, 0],
            cached_total_histogram: cached_histogram(&[0, 2, 2]),
            cached_signed_histogram: cached_signed_histogram(&[0, 1, 2], &[0, 1, 0]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };
        let histogram =
            compute_frame_histogram(&frame, PreviewMode::Intensity(Colormap::Grays), 30_000);
        assert_eq!(histogram, vec![1, 0, 2]);
    }

    #[test]
    fn prepared_intensity_payload_exposes_combined_pixels_and_geometry() {
        let frame = PreviewFrame {
            width: 2,
            height: 2,
            pixels: vec![1, 2, 3, 4],
            pixels_on: vec![0, 0, 0, 0],
            pixels_off: vec![0, 0, 0, 0],
            cached_total_histogram: cached_histogram(&[1, 2, 3, 4]),
            cached_signed_histogram: cached_signed_histogram(&[0, 0, 0, 0], &[0, 0, 0, 0]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };

        with_prepared_preview_frame(
            &frame,
            PreviewMode::Intensity(Colormap::Grays),
            30_000,
            |prepared| match prepared {
                PreparedPreviewFrame::IntensityR16 { size, values } => {
                    assert_eq!(size, [2, 2]);
                    assert_eq!(values, &[1, 2, 3, 4]);
                }
                other => panic!("unexpected payload: {other:?}"),
            },
        );
    }

    #[test]
    fn frame_to_color_image_renders_cpu_preview_without_overlay_compositing() {
        let frame = PreviewFrame {
            width: 2,
            height: 1,
            pixels: vec![24, 24],
            pixels_on: vec![20, 3],
            pixels_off: vec![4, 21],
            cached_total_histogram: cached_histogram(&[24, 24]),
            cached_signed_histogram: cached_signed_histogram(&[20, 3], &[4, 21]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };
        let image = frame_to_color_image(
            &frame,
            PreviewDisplaySettings::default(),
            PreviewMode::RedBlue,
            30_000,
        );

        assert_eq!(image.size, [2, 1]);
        assert_eq!(image.pixels.len(), 2);
        // ON-dominant pixel: hot magenta tint — strong R+B, weaker G.
        let on_px = image.pixels[0].to_array();
        assert!(on_px[0] > 0, "ON pixel red channel should be lit");
        assert!(on_px[2] > 0, "ON pixel blue channel should be lit");
        assert!(on_px[0] >= on_px[1], "magenta R should dominate over G");
        // OFF-dominant pixel: arctic cyan tint — no R, strong G+B.
        let off_px = image.pixels[1].to_array();
        assert_eq!(off_px[0], 0, "OFF pixel red channel should be dark");
        assert!(off_px[1] > 0, "OFF pixel green channel should be lit");
        assert!(off_px[2] > 0, "OFF pixel blue channel should be lit");
    }

    #[test]
    fn false_color_preview_uses_shared_lookup_tables() {
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![0],
            pixels_off: vec![0],
            cached_total_histogram: cached_histogram(&[1]),
            cached_signed_histogram: cached_signed_histogram(&[0], &[0]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };

        let image = frame_to_color_image(
            &frame,
            PreviewDisplaySettings {
                display_min: 0,
                display_max: 1,
                gamma: 1.0,
            },
            PreviewMode::Intensity(Colormap::Green),
            30_000,
        );

        assert_eq!(image.pixels[0], Colormap::Green.lookup(1.0));
    }

    #[test]
    fn signed_count_histogram_uses_net_polarity_magnitude() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![4, 6, 3],
            pixels_on: vec![4, 2, 1],
            pixels_off: vec![0, 5, 1],
            cached_total_histogram: cached_histogram(&[4, 6, 3]),
            cached_signed_histogram: cached_signed_histogram(&[4, 2, 1], &[0, 5, 1]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };

        let histogram = compute_frame_histogram(&frame, PreviewMode::SignedCount, 30_000);
        assert_eq!(histogram, vec![1, 0, 0, 1, 1]);
    }

    #[test]
    fn time_surface_histogram_uses_decayed_values_when_events_are_available() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            cached_total_histogram: cached_histogram(&[1]),
            cached_signed_histogram: cached_signed_histogram(&[1], &[0]),
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 10,
                polarity: true,
            }]),
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 10,
        };

        let histogram = compute_frame_histogram(&frame, PreviewMode::TimeSurface, 30_000);
        assert_eq!(histogram.len(), TIME_SURFACE_BINS);
        assert_eq!(histogram[255], 1);
    }

    #[test]
    fn prepared_time_surface_payload_uses_decayed_byte_cache() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            cached_total_histogram: cached_histogram(&[1]),
            cached_signed_histogram: cached_signed_histogram(&[1], &[0]),
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 25,
                polarity: true,
            }]),
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 25,
        };

        with_prepared_preview_frame(&frame, PreviewMode::TimeSurface, 30_000, |prepared| {
            match prepared {
                PreparedPreviewFrame::TimeSurfaceR8 { size, values } => {
                    assert_eq!(size, [1, 1]);
                    assert_eq!(values, &[255]);
                }
                other => panic!("unexpected payload: {other:?}"),
            }
        });
    }

    #[test]
    fn query_time_surface_value_uses_live_tick_state_when_decay_cache_is_missing() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            cached_total_histogram: cached_histogram(&[1]),
            cached_signed_histogram: cached_signed_histogram(&[1], &[0]),
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 25,
                polarity: true,
            }]),
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 25,
        };

        assert_eq!(query_time_surface_value(&frame, 30_000, 0), Some(255));
    }

    #[test]
    fn query_time_surface_value_returns_cached_decay_values() {
        reset_preview_render_cache();
        let frame = PreviewFrame {
            width: 1,
            height: 1,
            pixels: vec![1],
            pixels_on: vec![1],
            pixels_off: vec![0],
            cached_total_histogram: cached_histogram(&[1]),
            cached_signed_histogram: cached_signed_histogram(&[1], &[0]),
            on_count: 1,
            off_count: 0,
            events: Some(vec![CdEvent {
                x: 0,
                y: 0,
                timestamp: 10,
                polarity: true,
            }]),
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 10,
        };

        assert_eq!(query_time_surface_value(&frame, 30_000, 0), Some(255));

        let _ = compute_frame_histogram(&frame, PreviewMode::TimeSurface, 30_000);

        assert_eq!(query_time_surface_value(&frame, 30_000, 0), Some(255));
        assert_eq!(query_time_surface_value(&frame, 30_000, 1), None);
    }

    #[test]
    fn histogram_caps_hot_bins_to_keep_ui_work_bounded() {
        let frame = PreviewFrame {
            width: 3,
            height: 1,
            pixels: vec![0, 4095, 5000],
            pixels_on: vec![0, 4095, 5000],
            pixels_off: vec![0, 0, 0],
            cached_total_histogram: cached_histogram(&[0, 4095, 5000]),
            cached_signed_histogram: cached_signed_histogram(&[0, 4095, 5000], &[0, 0, 0]),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        };

        let histogram =
            compute_frame_histogram(&frame, PreviewMode::Intensity(Colormap::Grays), 30_000);
        assert_eq!(histogram.len(), MAX_HISTOGRAM_BINS);
        assert_eq!(histogram[0], 1);
        assert_eq!(histogram[MAX_HISTOGRAM_BINS - 1], 2);
    }
}
