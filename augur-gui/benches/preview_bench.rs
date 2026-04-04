#![allow(dead_code, unused_imports)]

use augur_core::pipeline::{CdEvent, PreviewFrame};
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

#[path = "../src/colormap.rs"]
mod colormap;
#[path = "../src/viewer_tools/line_profile.rs"]
mod line_profile;
#[path = "../src/preview.rs"]
mod preview;

use colormap::Colormap;
use line_profile::LineProfileTool;
use preview::{
    compute_frame_histogram, frame_to_color_image, reset_preview_render_cache,
    with_prepared_preview_frame, PreparedPreviewFrame, PreviewDisplaySettings, PreviewMode,
};

#[derive(Clone, Copy)]
enum FixtureDensity {
    Sparse,
    Medium,
    Dense,
}

impl FixtureDensity {
    fn label(self) -> &'static str {
        match self {
            Self::Sparse => "sparse",
            Self::Medium => "medium",
            Self::Dense => "dense",
        }
    }

    fn stride(self) -> usize {
        match self {
            Self::Sparse => 31,
            Self::Medium => 7,
            Self::Dense => 1,
        }
    }
}

fn synthetic_frame(width: u16, height: u16, density: FixtureDensity) -> PreviewFrame {
    let width_usize = usize::from(width.max(1));
    let height_usize = usize::from(height.max(1));
    let pixel_count = width_usize.saturating_mul(height_usize);
    let mut pixels = Vec::with_capacity(pixel_count);
    let mut pixels_on = Vec::with_capacity(pixel_count);
    let mut pixels_off = Vec::with_capacity(pixel_count);

    for index in 0..pixel_count {
        let x = index % width_usize;
        let y = index / width_usize;
        let on = ((x * 17 + y * 11 + index) % 512) as u16;
        let off = ((x * 5 + y * 23 + index * 3) % 512) as u16;
        pixels_on.push(on);
        pixels_off.push(off);
        pixels.push(on.saturating_add(off));
    }

    let mut events = Vec::new();
    let stride = density.stride();
    let mut timestamp = 1_u64;
    for y in (0..height_usize).step_by(stride) {
        for x in (0..width_usize).step_by(stride) {
            events.push(CdEvent {
                x: x as u16,
                y: y as u16,
                timestamp,
                polarity: ((x + y) & 1) == 0,
            });
            timestamp = timestamp.saturating_add(37);
        }
    }

    PreviewFrame {
        width,
        height,
        pixels_on,
        pixels_off,
        pixels,
        cached_total_histogram: Vec::new(),
        cached_signed_histogram: Vec::new(),
        on_count: 0,
        off_count: 0,
        events: Some(events),
        window_start_us: 0,
        window_end_us: timestamp.max(1),
    }
}

fn bench_frame_to_color_image(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_preview/frame_to_color_image");
    let settings = PreviewDisplaySettings {
        display_min: 0,
        display_max: 512,
        gamma: 0.85,
    };

    for &(width, height) in &[(320_u16, 240_u16), (1280_u16, 720_u16)] {
        for density in [
            FixtureDensity::Sparse,
            FixtureDensity::Medium,
            FixtureDensity::Dense,
        ] {
            let frame = synthetic_frame(width, height, density);
            group.bench_with_input(
                BenchmarkId::new(format!("{width}x{height}"), density.label()),
                &frame,
                |b, frame| {
                    b.iter(|| {
                        black_box(frame_to_color_image(
                            black_box(frame),
                            settings,
                            PreviewMode::Intensity(Colormap::Fire),
                            30_000,
                        ))
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_histogram(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_preview/histogram");
    for density in [
        FixtureDensity::Sparse,
        FixtureDensity::Medium,
        FixtureDensity::Dense,
    ] {
        let frame = synthetic_frame(1280, 720, density);
        group.bench_with_input(
            BenchmarkId::new("1280x720", density.label()),
            &frame,
            |b, frame| {
                b.iter(|| {
                    black_box(compute_frame_histogram(
                        black_box(frame),
                        PreviewMode::SignedCount,
                        30_000,
                    ))
                });
            },
        );
    }
    group.finish();
}

fn bench_time_surface_prepare(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_preview/time_surface_prepare");
    for density in [
        FixtureDensity::Sparse,
        FixtureDensity::Medium,
        FixtureDensity::Dense,
    ] {
        let frame = synthetic_frame(1280, 720, density);
        group.bench_with_input(
            BenchmarkId::new("1280x720", density.label()),
            &frame,
            |b, frame| {
                b.iter(|| {
                    reset_preview_render_cache();
                    with_prepared_preview_frame(
                        black_box(frame),
                        PreviewMode::TimeSurface,
                        30_000,
                        |prepared| match prepared {
                            PreparedPreviewFrame::TimeSurfaceR8 { values, .. } => {
                                black_box(values.len())
                            }
                            PreparedPreviewFrame::IntensityR16 { values, .. } => {
                                black_box(values.len())
                            }
                            PreparedPreviewFrame::PolarityRg16 { total, .. } => {
                                black_box(total.len())
                            }
                        },
                    )
                });
            },
        );
    }
    group.finish();
}

fn bench_line_profile_recompute(c: &mut Criterion) {
    let mut group = c.benchmark_group("cpu_preview/line_profile");
    for density in [
        FixtureDensity::Sparse,
        FixtureDensity::Medium,
        FixtureDensity::Dense,
    ] {
        let frame = synthetic_frame(1280, 720, density);
        group.bench_with_input(
            BenchmarkId::new("1280x720", density.label()),
            &frame,
            |b, frame| {
                let mut tool = LineProfileTool::default();
                tool.start = Some((0, 0));
                tool.end = Some((
                    frame.width.saturating_sub(1),
                    frame.height.saturating_sub(1),
                ));
                b.iter(|| {
                    tool.recompute(black_box(frame));
                    black_box((tool.profile_on.len(), tool.profile_off.len()))
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    preview_benches,
    bench_frame_to_color_image,
    bench_histogram,
    bench_time_surface_prepare,
    bench_line_profile_recompute
);
criterion_main!(preview_benches);
