use std::sync::Arc;
use std::time::Instant;

use augur_core::pipeline::PREVIEW_HISTOGRAM_BINS;
use bytemuck::{Pod, Zeroable};
use egui::{TextureHandle, TextureOptions};
use egui_wgpu::wgpu;

use crate::{
    colormap::Colormap,
    preview::{
        encode_time_surface_tick, query_time_surface_value, with_prepared_preview_frame,
        CpuPreviewImageCache, PreparedPreviewFrame, PreviewDisplaySettings, PreviewMode,
        TIME_SURFACE_BINS, TIME_SURFACE_TICK_US,
    },
    preview_perf::PreviewPerfStats,
    viewer_widget::PreviewHistogramRequest,
};

const MODE_INTENSITY: u32 = 0;
const MODE_RED_BLUE: u32 = 1;
const MODE_SIGNED_COUNT: u32 = 2;
const MODE_TIME_SURFACE: u32 = 3;
const COUNT_WORKGROUP_SIZE: u32 = 64;
const TIME_SURFACE_AUTO_CONTRAST_TARGET_SAMPLES: usize = 65_536;

fn uses_gpu_count_accumulation(mode: PreviewMode) -> bool {
    matches!(
        mode,
        PreviewMode::Intensity(_) | PreviewMode::RedBlue | PreviewMode::SignedCount
    )
}

const PREVIEW_SHADER: &str = r#"
struct PreviewUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    time_surface_tau_us: f32,
    time_surface_frame_end_tick: u32,
    time_surface_tick_us: u32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0)
var<uniform> uniforms: PreviewUniforms;
@group(0) @binding(1)
var intensity_tex: texture_2d<u32>;
@group(0) @binding(2)
var polarity_tex: texture_2d<u32>;
@group(0) @binding(3)
var timesurface_tex: texture_2d<u32>;
@group(0) @binding(4)
var lut_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn clamp_coord(uv: vec2<f32>) -> vec2<i32> {
    let width = max(uniforms.width, 1u);
    let height = max(uniforms.height, 1u);
    let x = min(u32(floor(clamp(uv.x, 0.0, 0.999999) * f32(width))), width - 1u);
    let y = min(u32(floor(clamp(uv.y, 0.0, 0.999999) * f32(height))), height - 1u);
    return vec2<i32>(i32(x), i32(y));
}

fn normalize_value(value: u32) -> f32 {
    let display_min = uniforms.display_min;
    let normalized = clamp((f32(value) - display_min) * uniforms.inverse_range, 0.0, 1.0);
    return pow(normalized, uniforms.gamma);
}

fn lut_color(row: u32, t: f32) -> vec3<f32> {
    let idx = min(u32(round(clamp(t, 0.0, 1.0) * 255.0)), 255u);
    return textureLoad(lut_tex, vec2<i32>(i32(idx), i32(row)), 0).rgb;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coord = clamp_coord(in.uv);
    if uniforms.mode == 0u {
        let value = textureLoad(intensity_tex, coord, 0).r;
        return vec4<f32>(lut_color(uniforms.colormap_row, normalize_value(value)), 1.0);
    }
    if uniforms.mode == 1u {
        let counts = textureLoad(polarity_tex, coord, 0).rg;
        let on = counts.r;
        let off = counts.g;
        let total = on + off;
        if total == 0u {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        let brightness = normalize_value(total);
        let polarity_on = vec3<f32>(0.9098, 0.2431, 0.6902);
        let polarity_off = vec3<f32>(0.0000, 0.7216, 0.8314);
        if on > off {
            return vec4<f32>(polarity_on * brightness, 1.0);
        }
        if off > on {
            return vec4<f32>(polarity_off * brightness, 1.0);
        }
        return vec4<f32>(((polarity_on + polarity_off) * 0.5) * brightness, 1.0);
    }
    if uniforms.mode == 2u {
        let counts = textureLoad(polarity_tex, coord, 0).rg;
        let on = counts.r;
        let off = counts.g;
        let total = on + off;
        if total == 0u {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        let magnitude = normalize_value(u32(abs(i32(on) - i32(off))));
        var signed_t = 0.5;
        if on > off {
            signed_t = 0.5 + 0.5 * magnitude;
        } else if off > on {
            signed_t = 0.5 - 0.5 * magnitude;
        }
        return vec4<f32>(lut_color(uniforms.colormap_row, signed_t), 1.0);
    }

    let last_tick = textureLoad(timesurface_tex, coord, 0).r;
    var decay_value = 0u;
    if last_tick > 0u {
        let dt_ticks = uniforms.time_surface_frame_end_tick - last_tick;
        let dt_us = f32(dt_ticks) * f32(uniforms.time_surface_tick_us);
        let decay = exp(-dt_us / max(uniforms.time_surface_tau_us, 1.0));
        decay_value = u32(round(clamp(decay, 0.0, 1.0) * 255.0));
    }
    return vec4<f32>(lut_color(uniforms.colormap_row, normalize_value(decay_value)), 1.0);
}
"#;

const COUNT_COMPUTE_SHADER: &str = r#"
struct CountUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    event_count: u32,
    dispatch_width: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0)
var<uniform> uniforms: CountUniforms;
@group(0) @binding(1)
var<storage, read> packed_events: array<u32>;
@group(0) @binding(2)
var<storage, read_write> total_counts: array<atomic<u32>>;
@group(0) @binding(3)
var<storage, read_write> on_counts: array<atomic<u32>>;
@group(0) @binding(4)
var<storage, read_write> off_counts: array<atomic<u32>>;
@group(0) @binding(5)
var<storage, read_write> histogram: array<atomic<u32>>;

fn packed_event_xy(event: u32) -> vec2<u32> {
    return vec2<u32>(event & 0xffffu, (event >> 16u) & 0x7fffu);
}

fn packed_event_polarity(event: u32) -> u32 {
    return (event >> 31u) & 1u;
}

fn pixel_index(x: u32, y: u32, width: u32) -> u32 {
    return y * width + x;
}

@compute @workgroup_size(64)
fn cs_accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    // The grid is two-dimensional once one row of workgroups is not enough to
    // cover the events; `dispatch_width` is that row's length in invocations.
    // For a single-row dispatch `gid.y` is 0 and this is just `gid.x`.
    let event_index = gid.x + gid.y * uniforms.dispatch_width;
    if event_index >= uniforms.event_count {
        return;
    }

    let packed = packed_events[event_index];
    let xy = packed_event_xy(packed);
    if xy.x >= uniforms.width || xy.y >= uniforms.height {
        return;
    }

    let index = pixel_index(xy.x, xy.y, uniforms.width);
    atomicAdd(&total_counts[index], 1u);
    if packed_event_polarity(packed) == 1u {
        atomicAdd(&on_counts[index], 1u);
    } else {
        atomicAdd(&off_counts[index], 1u);
    }
}

@compute @workgroup_size(64)
fn cs_histogram(@builtin(global_invocation_id) gid: vec3<u32>) {
    let index = gid.x + gid.y * uniforms.dispatch_width;
    let pixel_count = uniforms.width * uniforms.height;
    if index >= pixel_count {
        return;
    }

    let total = atomicLoad(&total_counts[index]);
    var bin = min(total, 4095u);
    if uniforms.mode == 2u {
        let on = atomicLoad(&on_counts[index]);
        let off = atomicLoad(&off_counts[index]);
        bin = min(u32(abs(i32(on) - i32(off))), 4095u);
    }
    atomicAdd(&histogram[bin], 1u);
}
"#;

const COUNT_RENDER_SHADER: &str = r#"
struct CountUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    event_count: u32,
    dispatch_width: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0)
var<uniform> uniforms: CountUniforms;
@group(0) @binding(1)
var<storage, read> total_counts_ro: array<u32>;
@group(0) @binding(2)
var<storage, read> on_counts_ro: array<u32>;
@group(0) @binding(3)
var<storage, read> off_counts_ro: array<u32>;
@group(0) @binding(4)
var lut_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn clamp_coord(uv: vec2<f32>) -> vec2<u32> {
    let width = max(uniforms.width, 1u);
    let height = max(uniforms.height, 1u);
    let x = min(u32(floor(clamp(uv.x, 0.0, 0.999999) * f32(width))), width - 1u);
    let y = min(u32(floor(clamp(uv.y, 0.0, 0.999999) * f32(height))), height - 1u);
    return vec2<u32>(x, y);
}

fn normalize_value(value: u32) -> f32 {
    let normalized = clamp(
        (f32(value) - uniforms.display_min) * uniforms.inverse_range,
        0.0,
        1.0,
    );
    return pow(normalized, uniforms.gamma);
}

fn lut_color(row: u32, t: f32) -> vec3<f32> {
    let idx = min(u32(round(clamp(t, 0.0, 1.0) * 255.0)), 255u);
    return textureLoad(lut_tex, vec2<i32>(i32(idx), i32(row)), 0).rgb;
}

fn pixel_index(x: u32, y: u32, width: u32) -> u32 {
    return y * width + x;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coord = clamp_coord(in.uv);
    let index = pixel_index(coord.x, coord.y, uniforms.width);
    let total = total_counts_ro[index];
    if uniforms.mode == 0u {
        return vec4<f32>(
            lut_color(uniforms.colormap_row, normalize_value(total)),
            1.0,
        );
    }

    let on = on_counts_ro[index];
    let off = off_counts_ro[index];
    if total == 0u {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    if uniforms.mode == 1u {
        let brightness = normalize_value(total);
        let polarity_on = vec3<f32>(0.9098, 0.2431, 0.6902);
        let polarity_off = vec3<f32>(0.0000, 0.7216, 0.8314);
        if on > off {
            return vec4<f32>(polarity_on * brightness, 1.0);
        }
        if off > on {
            return vec4<f32>(polarity_off * brightness, 1.0);
        }
        return vec4<f32>(((polarity_on + polarity_off) * 0.5) * brightness, 1.0);
    }

    let magnitude = normalize_value(u32(abs(i32(on) - i32(off))));
    var signed_t = 0.5;
    if on > off {
        signed_t = 0.5 + 0.5 * magnitude;
    } else if off > on {
        signed_t = 0.5 - 0.5 * magnitude;
    }
    return vec4<f32>(lut_color(uniforms.colormap_row, signed_t), 1.0);
}
"#;

const TIME_SURFACE_COMPUTE_SHADER: &str = r#"
struct TimeSurfaceAccumulateUniforms {
    width: u32,
    height: u32,
    event_count: u32,
    dispatch_width: u32,
};

struct AtomicValues {
    data: array<atomic<u32>>,
};

@group(0) @binding(0)
var<uniform> uniforms: TimeSurfaceAccumulateUniforms;
@group(0) @binding(1)
var<storage, read> packed_events: array<u32>;
@group(0) @binding(2)
var<storage, read_write> tick_buf: AtomicValues;

@compute @workgroup_size(64)
fn cs_accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    // See the count shader: `dispatch_width` is one grid row's worth of
    // invocations, and is 0-free even for a single-row dispatch.
    let event_index = gid.x + gid.y * uniforms.dispatch_width;
    if event_index >= uniforms.event_count {
        return;
    }

    let base = event_index * 2u;
    let packed_xy = packed_events[base];
    let tick = packed_events[base + 1u];
    let x = packed_xy & 0xffffu;
    let y = (packed_xy >> 16u) & 0xffffu;
    if x >= uniforms.width || y >= uniforms.height {
        return;
    }

    let pixel = y * uniforms.width + x;
    atomicMax(&tick_buf.data[pixel], tick);
}
"#;

const TIME_SURFACE_RENDER_SHADER: &str = r#"
struct TimeSurfaceRenderUniforms {
    width: u32,
    height: u32,
    colormap_row: u32,
    _pad0: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    time_surface_tau_us: f32,
    frame_end_tick: u32,
    tick_period_us: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0)
var<uniform> uniforms: TimeSurfaceRenderUniforms;
@group(0) @binding(1)
var<storage, read> tick_buf: array<u32>;
@group(0) @binding(2)
var lut_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VsOut {
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    var uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 2.0),
        vec2<f32>(0.0, 0.0),
        vec2<f32>(2.0, 0.0),
    );
    var out: VsOut;
    out.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    out.uv = uvs[vertex_index];
    return out;
}

fn clamp_coord(uv: vec2<f32>) -> vec2<u32> {
    let width = max(uniforms.width, 1u);
    let height = max(uniforms.height, 1u);
    let x = min(u32(floor(clamp(uv.x, 0.0, 0.999999) * f32(width))), width - 1u);
    let y = min(u32(floor(clamp(uv.y, 0.0, 0.999999) * f32(height))), height - 1u);
    return vec2<u32>(x, y);
}

fn normalize_value(value: u32) -> f32 {
    let normalized = clamp(
        (f32(value) - uniforms.display_min) * uniforms.inverse_range,
        0.0,
        1.0,
    );
    return pow(normalized, uniforms.gamma);
}

fn lut_color(row: u32, t: f32) -> vec3<f32> {
    let idx = min(u32(round(clamp(t, 0.0, 1.0) * 255.0)), 255u);
    return textureLoad(lut_tex, vec2<i32>(i32(idx), i32(row)), 0).rgb;
}

fn pixel_index(x: u32, y: u32, width: u32) -> u32 {
    return y * width + x;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coord = clamp_coord(in.uv);
    let index = pixel_index(coord.x, coord.y, uniforms.width);
    let last_tick = tick_buf[index];
    var value = 0u;
    if last_tick > 0u {
        let dt_ticks = uniforms.frame_end_tick - last_tick;
        let dt_us = f32(dt_ticks) * f32(uniforms.tick_period_us);
        let decay = exp(-dt_us / max(uniforms.time_surface_tau_us, 1.0));
        value = u32(round(clamp(decay, 0.0, 1.0) * 255.0));
    }
    return vec4<f32>(lut_color(uniforms.colormap_row, normalize_value(value)), 1.0);
}
"#;

const TIME_SURFACE_HISTOGRAM_SHADER: &str = r#"
struct TimeSurfaceHistogramUniforms {
    width: u32,
    height: u32,
    histogram_bin_count: u32,
    sample_stride: u32,
    frame_end_tick: u32,
    tick_period_us: u32,
    dispatch_width: u32,
    _pad1: u32,
    time_surface_tau_us: f32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
};

struct Values {
    data: array<u32>,
};

struct AtomicValues {
    data: array<atomic<u32>>,
};

@group(0) @binding(0)
var<uniform> uniforms: TimeSurfaceHistogramUniforms;
@group(0) @binding(1)
var<storage, read> tick_buf: Values;
@group(0) @binding(2)
var<storage, read_write> histogram_buf: AtomicValues;

fn decay_value(last_tick: u32) -> u32 {
    if last_tick == 0u {
        return 0u;
    }
    let dt_ticks = uniforms.frame_end_tick - last_tick;
    let dt_us = f32(dt_ticks) * f32(uniforms.tick_period_us);
    let decay = exp(-dt_us / max(uniforms.time_surface_tau_us, 1.0));
    return u32(round(clamp(decay, 0.0, 1.0) * 255.0));
}

@compute @workgroup_size(64)
fn cs_histogram(@builtin(global_invocation_id) gid: vec3<u32>) {
    let stride = max(uniforms.sample_stride, 1u);
    let index = (gid.x + gid.y * uniforms.dispatch_width) * stride;
    let pixel_count = uniforms.width * uniforms.height;
    if index >= pixel_count {
        return;
    }

    let bin_count = max(uniforms.histogram_bin_count, 1u);
    let bin = min(decay_value(tick_buf.data[index]), bin_count - 1u);
    atomicAdd(&histogram_buf.data[bin], 1u);
}
"#;

#[derive(Clone)]
pub enum PreviewDisplayTexture {
    Managed(TextureHandle),
    Native {
        id: egui::TextureId,
        size: [usize; 2],
    },
}

impl PreviewDisplayTexture {
    pub fn paint_at(&self, ui: &mut egui::Ui, rect: egui::Rect, uv: egui::Rect) {
        match self {
            Self::Managed(handle) => {
                egui::Image::new(handle).uv(uv).paint_at(ui, rect);
            }
            Self::Native { id, size } => {
                egui::Image::new((*id, egui::vec2(size[0] as f32, size[1] as f32)))
                    .uv(uv)
                    .paint_at(ui, rect);
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct PreviewRenderRequest<'a> {
    pub ctx: &'a egui::Context,
    pub frame: &'a augur_core::pipeline::PreviewFrame,
    pub settings: PreviewDisplaySettings,
    pub mode: PreviewMode,
    pub time_surface_tau_us: u64,
}

pub enum PreviewRenderer {
    Cpu(CpuPreviewRenderer),
    Wgpu(Box<WgpuPreviewRenderer>),
}

impl PreviewRenderer {
    pub fn new(cc: &eframe::CreationContext<'_>) -> (Self, Option<String>) {
        if let Some(render_state) = cc.wgpu_render_state.clone() {
            match WgpuPreviewRenderer::new(render_state) {
                Ok(renderer) => return (Self::Wgpu(Box::new(renderer)), None),
                Err(err) => {
                    return (
                        Self::Cpu(CpuPreviewRenderer::default()),
                        Some(format!("WGPU preview renderer disabled: {err}")),
                    );
                }
            }
        }

        (Self::Cpu(CpuPreviewRenderer::default()), None)
    }

    pub fn render(
        &mut self,
        request: PreviewRenderRequest<'_>,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        match self {
            Self::Cpu(renderer) => renderer.render(request, perf),
            Self::Wgpu(renderer) => renderer.render(request, perf),
        }
    }

    pub fn prefers_raw_events(&self, mode: PreviewMode) -> bool {
        matches!(self, Self::Wgpu(_)) && uses_gpu_count_accumulation(mode)
    }

    pub fn compute_histogram(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        mode: PreviewMode,
        time_surface_tau_us: u64,
        histogram_request: PreviewHistogramRequest,
    ) -> Result<Option<Vec<u64>>, String> {
        match self {
            Self::Cpu(_) => Ok(None),
            Self::Wgpu(renderer) => {
                renderer.compute_histogram(frame, mode, time_surface_tau_us, histogram_request)
            }
        }
    }

    pub fn query_time_surface_value(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        time_surface_tau_us: u64,
        index: usize,
    ) -> Option<u8> {
        match self {
            Self::Cpu(renderer) => {
                renderer.query_time_surface_value(frame, time_surface_tau_us, index)
            }
            Self::Wgpu(renderer) => {
                renderer.query_time_surface_value(frame, time_surface_tau_us, index)
            }
        }
    }

    pub fn reset(&mut self) {
        match self {
            Self::Cpu(renderer) => renderer.reset(),
            Self::Wgpu(renderer) => renderer.reset(),
        }
    }

    pub fn cpu_fallback() -> Self {
        Self::Cpu(CpuPreviewRenderer::default())
    }

    pub fn is_wgpu(&self) -> bool {
        matches!(self, Self::Wgpu(_))
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Cpu(_) => "cpu-fallback",
            Self::Wgpu(_) => "wgpu-preview",
        }
    }
}

#[derive(Default)]
pub struct CpuPreviewRenderer {
    texture: Option<TextureHandle>,
    cache: CpuPreviewImageCache,
}

impl CpuPreviewRenderer {
    fn render(
        &mut self,
        request: PreviewRenderRequest<'_>,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        let PreviewRenderRequest {
            ctx,
            frame,
            settings,
            mode,
            time_surface_tau_us,
        } = request;
        let render_started = Instant::now();
        let image = with_prepared_preview_frame(frame, mode, time_surface_tau_us, |prepared| {
            self.cache.render_prepared(prepared, settings, mode).clone()
        });
        perf.record_cpu_fallback_render(render_started.elapsed());

        let submit_started = Instant::now();
        if let Some(texture) = &mut self.texture {
            texture.set(image, TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR));
        }
        perf.record_upload_submit(submit_started.elapsed());

        self.texture
            .as_ref()
            .cloned()
            .map(PreviewDisplayTexture::Managed)
            .ok_or_else(|| "preview texture upload failed".to_owned())
    }

    fn reset(&mut self) {
        self.texture = None;
        self.cache = CpuPreviewImageCache::default();
    }

    fn query_time_surface_value(
        &self,
        frame: &augur_core::pipeline::PreviewFrame,
        time_surface_tau_us: u64,
        index: usize,
    ) -> Option<u8> {
        query_time_surface_value(frame, time_surface_tau_us, index)
    }
}

#[derive(Debug, Clone, Copy)]
struct IntensityR16<'a> {
    size: [usize; 2],
    values: &'a [u16],
}

#[derive(Debug, Clone, Copy)]
enum PackedPreviewPayload<'a> {
    Intensity(IntensityR16<'a>),
    Polarity { size: [usize; 2] },
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct PreviewUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    time_surface_tau_us: f32,
    time_surface_frame_end_tick: u32,
    time_surface_tick_us: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CountPreviewUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    event_count: u32,
    /// Invocations per grid row, so the shader can linearize a 2D dispatch.
    /// Padded to 48 bytes: a uniform-address-space struct is 16-byte aligned,
    /// and the WGSL declarations must match this layout field for field.
    dispatch_width: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct TimeSurfaceAccumulateUniforms {
    width: u32,
    height: u32,
    event_count: u32,
    /// Was padding, now carries the 2D grid's row length. Same 16 bytes.
    dispatch_width: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct TimeSurfaceRenderUniforms {
    width: u32,
    height: u32,
    colormap_row: u32,
    _pad0: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    time_surface_tau_us: f32,
    frame_end_tick: u32,
    tick_period_us: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct TimeSurfaceHistogramUniforms {
    width: u32,
    height: u32,
    histogram_bin_count: u32,
    sample_stride: u32,
    frame_end_tick: u32,
    tick_period_us: u32,
    dispatch_width: u32,
    _pad1: u32,
    time_surface_tau_us: f32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CountAccumulationKey {
    width: u16,
    height: u16,
    window_start_us: u64,
    window_end_us: u64,
    event_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimeSurfaceAccumulationKey {
    width: u16,
    height: u16,
    frame_end_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TimeSurfaceHoverCache {
    frame_end_us: u64,
    tau_us: u64,
    index: usize,
    value: u8,
}

pub struct WgpuPreviewRenderer {
    render_state: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    count_compute_pipeline: wgpu::ComputePipeline,
    count_histogram_pipeline: wgpu::ComputePipeline,
    count_compute_bind_group_layout: wgpu::BindGroupLayout,
    count_render_pipeline: wgpu::RenderPipeline,
    count_render_bind_group_layout: wgpu::BindGroupLayout,
    count_uniform_buffer: wgpu::Buffer,
    time_surface_compute_pipeline: wgpu::ComputePipeline,
    time_surface_histogram_pipeline: wgpu::ComputePipeline,
    time_surface_render_pipeline: wgpu::RenderPipeline,
    time_surface_compute_bind_group_layout: wgpu::BindGroupLayout,
    time_surface_histogram_bind_group_layout: wgpu::BindGroupLayout,
    time_surface_render_bind_group_layout: wgpu::BindGroupLayout,
    time_surface_accumulate_uniform_buffer: wgpu::Buffer,
    time_surface_histogram_uniform_buffer: wgpu::Buffer,
    time_surface_render_uniform_buffer: wgpu::Buffer,
    count_compute_bind_group: Option<wgpu::BindGroup>,
    count_render_bind_group: Option<wgpu::BindGroup>,
    count_event_buffer: Option<wgpu::Buffer>,
    count_event_capacity: usize,
    count_total_buffer: Option<wgpu::Buffer>,
    count_on_buffer: Option<wgpu::Buffer>,
    count_off_buffer: Option<wgpu::Buffer>,
    count_histogram_buffer: wgpu::Buffer,
    count_histogram_readback: wgpu::Buffer,
    count_packed_events: Vec<u32>,
    count_accumulation_key: Option<CountAccumulationKey>,
    time_surface_compute_bind_group: Option<wgpu::BindGroup>,
    time_surface_histogram_bind_group: Option<wgpu::BindGroup>,
    time_surface_render_bind_group: Option<wgpu::BindGroup>,
    time_surface_event_buffer: Option<wgpu::Buffer>,
    time_surface_event_capacity: usize,
    time_surface_tick_buffer: Option<wgpu::Buffer>,
    time_surface_histogram_buffer: wgpu::Buffer,
    time_surface_histogram_readback: wgpu::Buffer,
    time_surface_packed_events: Vec<u32>,
    time_surface_accumulation_key: Option<TimeSurfaceAccumulationKey>,
    time_surface_hover_cache: Option<TimeSurfaceHoverCache>,
    _lut_texture: wgpu::Texture,
    lut_view: wgpu::TextureView,
    _dummy_intensity_texture: wgpu::Texture,
    dummy_intensity_view: wgpu::TextureView,
    _dummy_polarity_texture: wgpu::Texture,
    dummy_polarity_view: wgpu::TextureView,
    _dummy_timesurface_texture: wgpu::Texture,
    dummy_timesurface_view: wgpu::TextureView,
    bind_group: Option<wgpu::BindGroup>,
    intensity_texture: Option<wgpu::Texture>,
    intensity_view: Option<wgpu::TextureView>,
    polarity_texture: Option<wgpu::Texture>,
    polarity_view: Option<wgpu::TextureView>,
    timesurface_texture: Option<wgpu::Texture>,
    timesurface_view: Option<wgpu::TextureView>,
    display_texture: Option<wgpu::Texture>,
    display_view: Option<wgpu::TextureView>,
    display_texture_id: Option<egui::TextureId>,
    size: Option<[usize; 2]>,
    polarity_payload: Vec<[u16; 2]>,
}

impl WgpuPreviewRenderer {
    fn device(&self) -> &wgpu::Device {
        &self.render_state.device
    }

    fn queue(&self) -> &wgpu::Queue {
        &self.render_state.queue
    }

    /// How to lay out a one-invocation-per-item compute dispatch on *this*
    /// adapter. Asked of the device rather than assumed: exceeding the limit is
    /// a validation panic, not a recoverable error, so the number has to be the
    /// real one.
    fn dispatch_grid(&self, items: u32) -> DispatchGrid {
        DispatchGrid::for_items(
            items,
            self.device().limits().max_compute_workgroups_per_dimension,
        )
    }

    fn display_texture_result(&self, size: [usize; 2]) -> Result<PreviewDisplayTexture, String> {
        self.display_texture_id
            .map(|id| PreviewDisplayTexture::Native { id, size })
            .ok_or_else(|| "missing preview texture id".to_owned())
    }

    fn encode_and_submit_render_pass(
        &self,
        label: &str,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
    ) -> Result<(), String> {
        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: self
                        .display_view
                        .as_ref()
                        .ok_or_else(|| "missing preview display view".to_owned())?,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            render_pass.set_pipeline(pipeline);
            render_pass.set_bind_group(0, bind_group, &[]);
            render_pass.draw(0..3, 0..1);
        }
        self.queue().submit(Some(encoder.finish()));
        Ok(())
    }

    fn new(render_state: egui_wgpu::RenderState) -> Result<Self, String> {
        let device = Arc::clone(&render_state.device);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("augur_preview_shader"),
            source: wgpu::ShaderSource::Wgsl(PREVIEW_SHADER.into()),
        });
        let count_compute_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("augur_count_compute_shader"),
            source: wgpu::ShaderSource::Wgsl(COUNT_COMPUTE_SHADER.into()),
        });
        let count_render_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("augur_count_render_shader"),
            source: wgpu::ShaderSource::Wgsl(COUNT_RENDER_SHADER.into()),
        });
        let time_surface_compute_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("augur_time_surface_compute_shader"),
                source: wgpu::ShaderSource::Wgsl(TIME_SURFACE_COMPUTE_SHADER.into()),
            });
        let time_surface_histogram_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("augur_time_surface_histogram_shader"),
                source: wgpu::ShaderSource::Wgsl(TIME_SURFACE_HISTOGRAM_SHADER.into()),
            });
        let time_surface_render_shader =
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("augur_time_surface_render_shader"),
                source: wgpu::ShaderSource::Wgsl(TIME_SURFACE_RENDER_SHADER.into()),
            });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("augur_preview_bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let count_compute_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("augur_count_compute_bind_group_layout"),
                entries: &[
                    storage_uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                    storage_buffer_entry(1, wgpu::ShaderStages::COMPUTE, true),
                    storage_buffer_entry(2, wgpu::ShaderStages::COMPUTE, false),
                    storage_buffer_entry(3, wgpu::ShaderStages::COMPUTE, false),
                    storage_buffer_entry(4, wgpu::ShaderStages::COMPUTE, false),
                    storage_buffer_entry(5, wgpu::ShaderStages::COMPUTE, false),
                ],
            });
        let count_render_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("augur_count_render_bind_group_layout"),
                entries: &[
                    storage_uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                    storage_buffer_entry(1, wgpu::ShaderStages::FRAGMENT, true),
                    storage_buffer_entry(2, wgpu::ShaderStages::FRAGMENT, true),
                    storage_buffer_entry(3, wgpu::ShaderStages::FRAGMENT, true),
                    wgpu::BindGroupLayoutEntry {
                        binding: 4,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });
        let time_surface_compute_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("augur_time_surface_compute_bind_group_layout"),
                entries: &[
                    storage_uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                    storage_buffer_entry(1, wgpu::ShaderStages::COMPUTE, true),
                    storage_buffer_entry(2, wgpu::ShaderStages::COMPUTE, false),
                ],
            });
        let time_surface_histogram_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("augur_time_surface_histogram_bind_group_layout"),
                entries: &[
                    storage_uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                    storage_buffer_entry(1, wgpu::ShaderStages::COMPUTE, true),
                    storage_buffer_entry(2, wgpu::ShaderStages::COMPUTE, false),
                ],
            });
        let time_surface_render_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("augur_time_surface_render_bind_group_layout"),
                entries: &[
                    storage_uniform_entry(0, wgpu::ShaderStages::FRAGMENT),
                    storage_buffer_entry(1, wgpu::ShaderStages::FRAGMENT, true),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            sample_type: wgpu::TextureSampleType::Float { filterable: false },
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("augur_preview_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });
        let count_compute_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("augur_count_compute_pipeline_layout"),
                bind_group_layouts: &[&count_compute_bind_group_layout],
                push_constant_ranges: &[],
            });
        let count_render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("augur_count_render_pipeline_layout"),
                bind_group_layouts: &[&count_render_bind_group_layout],
                push_constant_ranges: &[],
            });
        let time_surface_compute_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("augur_time_surface_compute_pipeline_layout"),
                bind_group_layouts: &[&time_surface_compute_bind_group_layout],
                push_constant_ranges: &[],
            });
        let time_surface_histogram_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("augur_time_surface_histogram_pipeline_layout"),
                bind_group_layouts: &[&time_surface_histogram_bind_group_layout],
                push_constant_ranges: &[],
            });
        let time_surface_render_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("augur_time_surface_render_pipeline_layout"),
                bind_group_layouts: &[&time_surface_render_bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("augur_preview_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });
        let count_compute_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("augur_count_accumulate_pipeline"),
                layout: Some(&count_compute_pipeline_layout),
                module: &count_compute_shader,
                entry_point: "cs_accumulate",
            });
        let count_histogram_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("augur_count_histogram_pipeline"),
                layout: Some(&count_compute_pipeline_layout),
                module: &count_compute_shader,
                entry_point: "cs_histogram",
            });
        let count_render_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("augur_count_preview_pipeline"),
                layout: Some(&count_render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &count_render_shader,
                    entry_point: "vs_main",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &count_render_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });
        let time_surface_compute_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("augur_time_surface_accumulate_pipeline"),
                layout: Some(&time_surface_compute_pipeline_layout),
                module: &time_surface_compute_shader,
                entry_point: "cs_accumulate",
            });
        let time_surface_histogram_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("augur_time_surface_histogram_pipeline"),
                layout: Some(&time_surface_histogram_pipeline_layout),
                module: &time_surface_histogram_shader,
                entry_point: "cs_histogram",
            });
        let time_surface_render_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("augur_time_surface_render_pipeline"),
                layout: Some(&time_surface_render_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &time_surface_render_shader,
                    entry_point: "vs_main",
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &time_surface_render_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
            });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_preview_uniforms"),
            size: std::mem::size_of::<PreviewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let count_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_count_preview_uniforms"),
            size: std::mem::size_of::<CountPreviewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let time_surface_accumulate_uniform_buffer =
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("augur_time_surface_accumulate_uniforms"),
                size: std::mem::size_of::<TimeSurfaceAccumulateUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        let time_surface_histogram_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_time_surface_histogram_uniforms"),
            size: std::mem::size_of::<TimeSurfaceHistogramUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let time_surface_render_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_time_surface_render_uniforms"),
            size: std::mem::size_of::<TimeSurfaceRenderUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let lut_texture = create_lut_texture(&device, &render_state.queue);
        let lut_view = lut_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_intensity_texture = create_dummy_texture(&device, wgpu::TextureFormat::R16Uint);
        let dummy_intensity_view =
            dummy_intensity_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_polarity_texture = create_dummy_texture(&device, wgpu::TextureFormat::Rg16Uint);
        let dummy_polarity_view =
            dummy_polarity_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_timesurface_texture = create_dummy_texture(&device, wgpu::TextureFormat::R32Uint);
        let dummy_timesurface_view =
            dummy_timesurface_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let count_histogram_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_count_histogram"),
            size: (PREVIEW_HISTOGRAM_BINS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let count_histogram_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_count_histogram_readback"),
            size: (PREVIEW_HISTOGRAM_BINS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let time_surface_histogram_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_time_surface_histogram"),
            size: (TIME_SURFACE_BINS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let time_surface_histogram_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_time_surface_histogram_readback"),
            size: (TIME_SURFACE_BINS * std::mem::size_of::<u32>()) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            render_state,
            pipeline,
            bind_group_layout,
            uniform_buffer,
            count_compute_pipeline,
            count_histogram_pipeline,
            count_compute_bind_group_layout,
            count_render_pipeline,
            count_render_bind_group_layout,
            count_uniform_buffer,
            time_surface_compute_pipeline,
            time_surface_histogram_pipeline,
            time_surface_render_pipeline,
            time_surface_compute_bind_group_layout,
            time_surface_histogram_bind_group_layout,
            time_surface_render_bind_group_layout,
            time_surface_accumulate_uniform_buffer,
            time_surface_histogram_uniform_buffer,
            time_surface_render_uniform_buffer,
            count_compute_bind_group: None,
            count_render_bind_group: None,
            count_event_buffer: None,
            count_event_capacity: 0,
            count_total_buffer: None,
            count_on_buffer: None,
            count_off_buffer: None,
            count_histogram_buffer,
            count_histogram_readback,
            count_packed_events: Vec::new(),
            count_accumulation_key: None,
            time_surface_compute_bind_group: None,
            time_surface_histogram_bind_group: None,
            time_surface_render_bind_group: None,
            time_surface_event_buffer: None,
            time_surface_event_capacity: 0,
            time_surface_tick_buffer: None,
            time_surface_histogram_buffer,
            time_surface_histogram_readback,
            time_surface_packed_events: Vec::new(),
            time_surface_accumulation_key: None,
            time_surface_hover_cache: None,
            _lut_texture: lut_texture,
            lut_view,
            _dummy_intensity_texture: dummy_intensity_texture,
            dummy_intensity_view,
            _dummy_polarity_texture: dummy_polarity_texture,
            dummy_polarity_view,
            _dummy_timesurface_texture: dummy_timesurface_texture,
            dummy_timesurface_view,
            bind_group: None,
            intensity_texture: None,
            intensity_view: None,
            polarity_texture: None,
            polarity_view: None,
            timesurface_texture: None,
            timesurface_view: None,
            display_texture: None,
            display_view: None,
            display_texture_id: None,
            size: None,
            polarity_payload: Vec::new(),
        })
    }

    fn render(
        &mut self,
        request: PreviewRenderRequest<'_>,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        let PreviewRenderRequest {
            frame,
            settings,
            mode,
            time_surface_tau_us,
            ..
        } = request;
        if uses_gpu_count_accumulation(mode) && frame.raw_events_available() {
            return self.render_count_accumulated(frame, settings, mode, perf);
        }

        if matches!(mode, PreviewMode::TimeSurface) {
            if frame.raw_events_available() {
                return self.render_time_surface_accumulated(
                    frame,
                    settings,
                    time_surface_tau_us,
                    perf,
                );
            }

            return Err(
                "time-surface GPU preview requires raw preview events; switching to CPU fallback"
                    .to_owned(),
            );
        }

        with_prepared_preview_frame(frame, mode, time_surface_tau_us, |prepared| {
            let payload = self.pack_payload(prepared);
            self.render_packed_payload(payload, settings, mode, time_surface_tau_us, perf)
        })
    }

    fn compute_histogram(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        mode: PreviewMode,
        time_surface_tau_us: u64,
        histogram_request: PreviewHistogramRequest,
    ) -> Result<Option<Vec<u64>>, String> {
        if histogram_request == PreviewHistogramRequest::None {
            return Ok(None);
        }
        if matches!(mode, PreviewMode::TimeSurface) && frame.raw_events_available() {
            return self
                .compute_time_surface_histogram(frame, time_surface_tau_us, histogram_request)
                .map(Some);
        }
        if !uses_gpu_count_accumulation(mode) || !frame.raw_events_available() {
            return Ok(None);
        }

        self.compute_count_histogram(frame, mode).map(Some)
    }

    fn query_time_surface_value(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        time_surface_tau_us: u64,
        index: usize,
    ) -> Option<u8> {
        // Use the CPU fallback for hover queries to avoid stalling the GPU
        // pipeline with a single-pixel readback.
        query_time_surface_value(frame, time_surface_tau_us, index)
    }

    fn render_time_surface_accumulated(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        settings: PreviewDisplaySettings,
        time_surface_tau_us: u64,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        self.ensure_time_surface_accumulation(frame)?;

        let size = [frame.width as usize, frame.height as usize];
        self.ensure_textures(size)?;
        self.ensure_time_surface_render_bind_group()?;
        self.queue().write_buffer(
            &self.time_surface_render_uniform_buffer,
            0,
            bytemuck::bytes_of(&time_surface_render_uniforms(
                size,
                settings,
                frame.window_end_us,
                time_surface_tau_us,
            )),
        );

        let submit_started = Instant::now();
        let bind_group = self
            .time_surface_render_bind_group
            .as_ref()
            .ok_or_else(|| "missing time-surface render bind group".to_owned())?;
        self.encode_and_submit_render_pass(
            "augur_time_surface_render",
            &self.time_surface_render_pipeline,
            bind_group,
        )?;
        perf.record_upload_submit(submit_started.elapsed());

        self.display_texture_result(size)
    }

    fn compute_time_surface_histogram(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        time_surface_tau_us: u64,
        histogram_request: PreviewHistogramRequest,
    ) -> Result<Vec<u64>, String> {
        self.ensure_time_surface_accumulation(frame)?;
        self.ensure_time_surface_histogram_bind_group()?;

        let size = [frame.width as usize, frame.height as usize];
        let pixel_count = size[0].saturating_mul(size[1]).max(1);
        let sample_stride = match histogram_request {
            PreviewHistogramRequest::AutoContrast => pixel_count
                .div_ceil(TIME_SURFACE_AUTO_CONTRAST_TARGET_SAMPLES)
                .max(1),
            PreviewHistogramRequest::Full | PreviewHistogramRequest::None => 1,
        };

        let sampled_items = pixel_count.div_ceil(sample_stride) as u32;
        let grid = self.dispatch_grid(sampled_items);
        self.queue().write_buffer(
            &self.time_surface_histogram_uniform_buffer,
            0,
            bytemuck::bytes_of(&time_surface_histogram_uniforms(
                size,
                frame.window_end_us,
                time_surface_tau_us,
                sample_stride,
                grid.span,
            )),
        );

        let histogram_bytes = (TIME_SURFACE_BINS * std::mem::size_of::<u32>()) as u64;
        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("augur_time_surface_histogram_encoder"),
            });
        encoder.clear_buffer(&self.time_surface_histogram_buffer, 0, None);
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("augur_time_surface_histogram_pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.time_surface_histogram_pipeline);
            compute_pass.set_bind_group(
                0,
                self.time_surface_histogram_bind_group
                    .as_ref()
                    .ok_or_else(|| "missing time-surface histogram bind group".to_owned())?,
                &[],
            );
            compute_pass.dispatch_workgroups(grid.x, grid.y, 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.time_surface_histogram_buffer,
            0,
            &self.time_surface_histogram_readback,
            0,
            histogram_bytes,
        );
        self.queue().submit(Some(encoder.finish()));

        read_histogram_buffer(
            self.device(),
            &self.time_surface_histogram_readback,
            TIME_SURFACE_BINS,
        )
    }

    fn render_count_accumulated(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        self.ensure_count_accumulation(frame, mode)?;

        let size = [frame.width as usize, frame.height as usize];
        self.ensure_textures(size)?;
        self.queue().write_buffer(
            &self.count_uniform_buffer,
            0,
            bytemuck::bytes_of(&count_preview_uniforms(
                mode,
                size,
                settings,
                frame.event_count().unwrap_or(0),
                // No compute pass follows this write — the render pipeline
                // reads the same buffer and never indexes by dispatch.
                0,
            )),
        );
        self.ensure_count_render_bind_group()?;

        let submit_started = Instant::now();
        let bind_group = self
            .count_render_bind_group
            .as_ref()
            .ok_or_else(|| "missing count render bind group".to_owned())?;
        self.encode_and_submit_render_pass(
            "augur_count_preview_render",
            &self.count_render_pipeline,
            bind_group,
        )?;
        perf.record_upload_submit(submit_started.elapsed());

        self.display_texture_result(size)
    }

    fn compute_count_histogram(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        mode: PreviewMode,
    ) -> Result<Vec<u64>, String> {
        self.ensure_count_accumulation(frame, mode)?;
        let size = [frame.width as usize, frame.height as usize];
        // One invocation per pixel here, not per event.
        let grid = self.dispatch_grid(size[0].saturating_mul(size[1]) as u32);
        self.queue().write_buffer(
            &self.count_uniform_buffer,
            0,
            bytemuck::bytes_of(&count_preview_uniforms(
                mode,
                size,
                PreviewDisplaySettings::default(),
                frame.event_count().unwrap_or(0),
                grid.span,
            )),
        );

        let histogram_bytes = (PREVIEW_HISTOGRAM_BINS * std::mem::size_of::<u32>()) as u64;
        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("augur_count_histogram_encoder"),
            });
        encoder.clear_buffer(&self.count_histogram_buffer, 0, None);
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("augur_count_histogram_pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.count_histogram_pipeline);
            compute_pass.set_bind_group(
                0,
                self.count_compute_bind_group
                    .as_ref()
                    .ok_or_else(|| "missing count compute bind group".to_owned())?,
                &[],
            );
            compute_pass.dispatch_workgroups(grid.x, grid.y, 1);
        }
        encoder.copy_buffer_to_buffer(
            &self.count_histogram_buffer,
            0,
            &self.count_histogram_readback,
            0,
            histogram_bytes,
        );
        self.queue().submit(Some(encoder.finish()));

        read_histogram_buffer(
            self.device(),
            &self.count_histogram_readback,
            histogram_bytes as usize / std::mem::size_of::<u32>(),
        )
    }

    fn render_packed_payload(
        &mut self,
        payload: PackedPreviewPayload<'_>,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
        time_surface_tau_us: u64,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        let size = match payload {
            PackedPreviewPayload::Intensity(data) => data.size,
            PackedPreviewPayload::Polarity { size, .. } => size,
        };
        self.ensure_textures(size)?;
        self.ensure_bind_group();

        let submit_started = Instant::now();
        self.upload_payload(payload);
        self.queue().write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&preview_uniforms(mode, size, settings, time_surface_tau_us)),
        );

        let bind_group = self
            .bind_group
            .as_ref()
            .ok_or_else(|| "missing preview bind group".to_owned())?;
        self.encode_and_submit_render_pass("augur_preview_render", &self.pipeline, bind_group)?;
        perf.record_upload_submit(submit_started.elapsed());

        self.display_texture_result(size)
    }

    fn ensure_count_accumulation(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
        mode: PreviewMode,
    ) -> Result<(), String> {
        let event_count = frame
            .event_count()
            .ok_or_else(|| "count accumulation requires raw preview events".to_owned())?;
        let key = CountAccumulationKey {
            width: frame.width,
            height: frame.height,
            window_start_us: frame.window_start_us,
            window_end_us: frame.window_end_us,
            event_count,
        };
        if self.count_accumulation_key == Some(key) {
            return Ok(());
        }

        let events = frame
            .events_snapshot()
            .ok_or_else(|| "count accumulation requires raw preview events".to_owned())?;
        let size = [frame.width as usize, frame.height as usize];
        self.ensure_count_buffers(size, events.len())?;

        self.count_packed_events.clear();
        self.count_packed_events.reserve(events.len());
        self.count_packed_events
            .extend(events.iter().map(pack_count_event));

        let event_buffer = self
            .count_event_buffer
            .as_ref()
            .ok_or_else(|| "missing count event buffer".to_owned())?;
        self.queue().write_buffer(
            event_buffer,
            0,
            bytemuck::cast_slice(&self.count_packed_events),
        );
        // One invocation per event. A high-contrast source can deliver
        // millions of events in a single preview frame.
        let grid = self.dispatch_grid(events.len() as u32);
        self.queue().write_buffer(
            &self.count_uniform_buffer,
            0,
            bytemuck::bytes_of(&count_preview_uniforms(
                mode,
                size,
                PreviewDisplaySettings::default(),
                events.len(),
                grid.span,
            )),
        );

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("augur_count_accumulate_encoder"),
            });
        encoder.clear_buffer(
            self.count_total_buffer
                .as_ref()
                .ok_or_else(|| "missing count total buffer".to_owned())?,
            0,
            None,
        );
        encoder.clear_buffer(
            self.count_on_buffer
                .as_ref()
                .ok_or_else(|| "missing count on buffer".to_owned())?,
            0,
            None,
        );
        encoder.clear_buffer(
            self.count_off_buffer
                .as_ref()
                .ok_or_else(|| "missing count off buffer".to_owned())?,
            0,
            None,
        );
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("augur_count_accumulate_pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.count_compute_pipeline);
            compute_pass.set_bind_group(
                0,
                self.count_compute_bind_group
                    .as_ref()
                    .ok_or_else(|| "missing count compute bind group".to_owned())?,
                &[],
            );
            compute_pass.dispatch_workgroups(grid.x, grid.y, 1);
        }
        self.queue().submit(Some(encoder.finish()));
        self.count_accumulation_key = Some(key);
        Ok(())
    }

    fn ensure_count_buffers(
        &mut self,
        size: [usize; 2],
        event_capacity: usize,
    ) -> Result<(), String> {
        let pixel_count = size[0].saturating_mul(size[1]).max(1);
        let count_buffer_size = (pixel_count * std::mem::size_of::<u32>()) as u64;
        let needs_count_buffers = self.size != Some(size)
            || self.count_total_buffer.is_none()
            || self.count_on_buffer.is_none()
            || self.count_off_buffer.is_none();
        if needs_count_buffers {
            let usage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST;
            self.count_total_buffer = Some(create_storage_buffer(
                &self.render_state.device,
                count_buffer_size,
                "augur_count_total",
                usage,
            ));
            self.count_on_buffer = Some(create_storage_buffer(
                &self.render_state.device,
                count_buffer_size,
                "augur_count_on",
                usage,
            ));
            self.count_off_buffer = Some(create_storage_buffer(
                &self.render_state.device,
                count_buffer_size,
                "augur_count_off",
                usage,
            ));
            self.count_render_bind_group = None;
            self.count_compute_bind_group = None;
            self.count_accumulation_key = None;
        }

        if event_capacity > self.count_event_capacity || self.count_event_buffer.is_none() {
            let capacity = event_capacity.max(1).next_power_of_two();
            self.count_event_buffer = Some(self.render_state.device.create_buffer(
                &wgpu::BufferDescriptor {
                    label: Some("augur_count_events"),
                    size: (capacity * std::mem::size_of::<u32>()) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
            self.count_event_capacity = capacity;
            self.count_compute_bind_group = None;
            self.count_accumulation_key = None;
        }

        self.ensure_count_compute_bind_group()?;
        Ok(())
    }

    fn ensure_count_compute_bind_group(&mut self) -> Result<(), String> {
        if self.count_compute_bind_group.is_some() {
            return Ok(());
        }
        self.count_compute_bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_count_compute_bind_group"),
                layout: &self.count_compute_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.count_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .count_event_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count event buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self
                            .count_total_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count total buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self
                            .count_on_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count on buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: self
                            .count_off_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count off buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: self.count_histogram_buffer.as_entire_binding(),
                    },
                ],
            }),
        );
        Ok(())
    }

    fn ensure_count_render_bind_group(&mut self) -> Result<(), String> {
        if self.count_render_bind_group.is_some() {
            return Ok(());
        }
        self.count_render_bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_count_render_bind_group"),
                layout: &self.count_render_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.count_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .count_total_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count total buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self
                            .count_on_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count on buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self
                            .count_off_buffer
                            .as_ref()
                            .ok_or_else(|| "missing count off buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&self.lut_view),
                    },
                ],
            }),
        );
        Ok(())
    }

    fn ensure_time_surface_accumulation(
        &mut self,
        frame: &augur_core::pipeline::PreviewFrame,
    ) -> Result<(), String> {
        let event_count = frame
            .event_count()
            .ok_or_else(|| "time-surface accumulation requires raw preview events".to_owned())?;
        let size = [frame.width as usize, frame.height as usize];
        self.ensure_time_surface_buffers(size, event_count)?;

        let accumulation_key = TimeSurfaceAccumulationKey {
            width: frame.width,
            height: frame.height,
            frame_end_us: frame.window_end_us,
        };
        let reset_needed = self.time_surface_accumulation_key.is_none_or(|key| {
            key.width != accumulation_key.width
                || key.height != accumulation_key.height
                || accumulation_key.frame_end_us < key.frame_end_us
        });
        if reset_needed {
            let mut encoder =
                self.device()
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("augur_time_surface_reset_encoder"),
                    });
            encoder.clear_buffer(
                self.time_surface_tick_buffer
                    .as_ref()
                    .ok_or_else(|| "missing time-surface tick buffer".to_owned())?,
                0,
                None,
            );
            self.queue().submit(Some(encoder.finish()));
            self.time_surface_accumulation_key = None;
            self.time_surface_hover_cache = None;
        }

        if self.time_surface_accumulation_key == Some(accumulation_key) {
            return Ok(());
        }

        let events = frame
            .events_snapshot()
            .ok_or_else(|| "time-surface accumulation requires raw preview events".to_owned())?;
        self.time_surface_packed_events.clear();
        self.time_surface_packed_events.reserve(events.len() * 2);
        self.time_surface_packed_events
            .extend(events.iter().flat_map(pack_time_surface_event));

        self.queue().write_buffer(
            self.time_surface_event_buffer
                .as_ref()
                .ok_or_else(|| "missing time-surface event buffer".to_owned())?,
            0,
            bytemuck::cast_slice(&self.time_surface_packed_events),
        );
        // The time-surface accumulate is the same shape, and was the same bug.
        let grid = self.dispatch_grid(events.len() as u32);
        self.queue().write_buffer(
            &self.time_surface_accumulate_uniform_buffer,
            0,
            bytemuck::bytes_of(&time_surface_accumulate_uniforms(
                size,
                events.len(),
                grid.span,
            )),
        );

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("augur_time_surface_accumulate_encoder"),
            });
        {
            let mut compute_pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("augur_time_surface_accumulate_pass"),
                timestamp_writes: None,
            });
            compute_pass.set_pipeline(&self.time_surface_compute_pipeline);
            compute_pass.set_bind_group(
                0,
                self.time_surface_compute_bind_group
                    .as_ref()
                    .ok_or_else(|| "missing time-surface compute bind group".to_owned())?,
                &[],
            );
            compute_pass.dispatch_workgroups(grid.x, grid.y, 1);
        }
        self.queue().submit(Some(encoder.finish()));
        self.time_surface_accumulation_key = Some(accumulation_key);
        self.time_surface_hover_cache = None;
        Ok(())
    }

    fn ensure_time_surface_buffers(
        &mut self,
        size: [usize; 2],
        event_capacity: usize,
    ) -> Result<(), String> {
        let pixel_count = size[0].saturating_mul(size[1]).max(1);
        let tick_buffer_size = (pixel_count * std::mem::size_of::<u32>()) as u64;
        if self.size != Some(size) || self.time_surface_tick_buffer.is_none() {
            self.time_surface_tick_buffer = Some(create_storage_buffer(
                &self.render_state.device,
                tick_buffer_size,
                "augur_time_surface_ticks",
                wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            ));
            self.time_surface_compute_bind_group = None;
            self.time_surface_histogram_bind_group = None;
            self.time_surface_render_bind_group = None;
            self.time_surface_accumulation_key = None;
            self.time_surface_hover_cache = None;
        }

        if event_capacity > self.time_surface_event_capacity
            || self.time_surface_event_buffer.is_none()
        {
            let capacity = event_capacity.max(1).next_power_of_two();
            self.time_surface_event_buffer = Some(self.render_state.device.create_buffer(
                &wgpu::BufferDescriptor {
                    label: Some("augur_time_surface_events"),
                    size: (capacity * 2 * std::mem::size_of::<u32>()) as u64,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
            self.time_surface_event_capacity = capacity;
            self.time_surface_compute_bind_group = None;
            self.time_surface_accumulation_key = None;
            self.time_surface_hover_cache = None;
        }

        self.ensure_time_surface_compute_bind_group()?;
        self.ensure_time_surface_histogram_bind_group()?;
        self.ensure_time_surface_render_bind_group()?;
        Ok(())
    }

    fn ensure_time_surface_compute_bind_group(&mut self) -> Result<(), String> {
        if self.time_surface_compute_bind_group.is_some() {
            return Ok(());
        }
        self.time_surface_compute_bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_time_surface_compute_bind_group"),
                layout: &self.time_surface_compute_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self
                            .time_surface_accumulate_uniform_buffer
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .time_surface_event_buffer
                            .as_ref()
                            .ok_or_else(|| "missing time-surface event buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self
                            .time_surface_tick_buffer
                            .as_ref()
                            .ok_or_else(|| "missing time-surface tick buffer".to_owned())?
                            .as_entire_binding(),
                    },
                ],
            }),
        );
        Ok(())
    }

    fn ensure_time_surface_histogram_bind_group(&mut self) -> Result<(), String> {
        if self.time_surface_histogram_bind_group.is_some() {
            return Ok(());
        }
        self.time_surface_histogram_bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_time_surface_histogram_bind_group"),
                layout: &self.time_surface_histogram_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self
                            .time_surface_histogram_uniform_buffer
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .time_surface_tick_buffer
                            .as_ref()
                            .ok_or_else(|| "missing time-surface tick buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.time_surface_histogram_buffer.as_entire_binding(),
                    },
                ],
            }),
        );
        Ok(())
    }

    fn ensure_time_surface_render_bind_group(&mut self) -> Result<(), String> {
        if self.time_surface_render_bind_group.is_some() {
            return Ok(());
        }
        self.time_surface_render_bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_time_surface_render_bind_group"),
                layout: &self.time_surface_render_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.time_surface_render_uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self
                            .time_surface_tick_buffer
                            .as_ref()
                            .ok_or_else(|| "missing time-surface tick buffer".to_owned())?
                            .as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&self.lut_view),
                    },
                ],
            }),
        );
        Ok(())
    }

    fn reset(&mut self) {
        self.free_display_texture();
        self.bind_group = None;
        self.count_compute_bind_group = None;
        self.count_render_bind_group = None;
        self.time_surface_compute_bind_group = None;
        self.time_surface_histogram_bind_group = None;
        self.time_surface_render_bind_group = None;
        self.count_event_buffer = None;
        self.count_event_capacity = 0;
        self.count_total_buffer = None;
        self.count_on_buffer = None;
        self.count_off_buffer = None;
        self.count_packed_events.clear();
        self.count_accumulation_key = None;
        self.time_surface_event_buffer = None;
        self.time_surface_event_capacity = 0;
        self.time_surface_tick_buffer = None;
        self.time_surface_packed_events.clear();
        self.time_surface_accumulation_key = None;
        self.time_surface_hover_cache = None;
        self.intensity_texture = None;
        self.intensity_view = None;
        self.polarity_texture = None;
        self.polarity_view = None;
        self.timesurface_texture = None;
        self.timesurface_view = None;
        self.display_texture = None;
        self.display_view = None;
        self.size = None;
        self.polarity_payload.clear();
    }

    fn free_display_texture(&mut self) {
        let Some(id) = self.display_texture_id.take() else {
            return;
        };
        self.render_state.renderer.write().free_texture(&id);
    }

    fn pack_payload<'frame>(
        &mut self,
        prepared: PreparedPreviewFrame<'frame>,
    ) -> PackedPreviewPayload<'frame> {
        match prepared {
            PreparedPreviewFrame::IntensityR16 { size, values } => {
                PackedPreviewPayload::Intensity(IntensityR16 { size, values })
            }
            PreparedPreviewFrame::PolarityRg16 { size, on, off, .. } => {
                let pixel_count = size[0].saturating_mul(size[1]);
                self.polarity_payload.resize(pixel_count, [0_u16, 0_u16]);
                for ((packed, &on), &off) in self.polarity_payload.iter_mut().zip(on).zip(off) {
                    *packed = [on, off];
                }
                PackedPreviewPayload::Polarity { size }
            }
            PreparedPreviewFrame::TimeSurfaceR8 { .. } => {
                unreachable!("wgpu time-surface payloads should come from timestamp ticks")
            }
        }
    }

    fn ensure_textures(&mut self, size: [usize; 2]) -> Result<(), String> {
        if self.size == Some(size) && self.display_texture_id.is_some() {
            return Ok(());
        }

        let device = self.device();
        let intensity_texture = create_source_texture(
            device,
            size,
            wgpu::TextureFormat::R16Uint,
            "augur_preview_intensity",
        );
        let polarity_texture = create_source_texture(
            device,
            size,
            wgpu::TextureFormat::Rg16Uint,
            "augur_preview_polarity",
        );
        let timesurface_texture = create_source_texture(
            device,
            size,
            wgpu::TextureFormat::R32Uint,
            "augur_preview_timesurface",
        );
        let display_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("augur_preview_display"),
            size: wgpu::Extent3d {
                width: size[0].max(1) as u32,
                height: size[1].max(1) as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
        });
        let display_view = display_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let mut renderer = self.render_state.renderer.write();
        let display_texture_id = if let Some(existing) = self.display_texture_id {
            renderer.update_egui_texture_from_wgpu_texture(
                device,
                &display_view,
                wgpu::FilterMode::Linear,
                existing,
            );
            existing
        } else {
            renderer.register_native_texture(device, &display_view, wgpu::FilterMode::Linear)
        };
        drop(renderer);

        self.intensity_view =
            Some(intensity_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        self.intensity_texture = Some(intensity_texture);
        self.polarity_view =
            Some(polarity_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        self.polarity_texture = Some(polarity_texture);
        self.timesurface_view =
            Some(timesurface_texture.create_view(&wgpu::TextureViewDescriptor::default()));
        self.timesurface_texture = Some(timesurface_texture);
        self.display_view = Some(display_view);
        self.display_texture = Some(display_texture);
        self.display_texture_id = Some(display_texture_id);
        self.bind_group = None;
        self.size = Some(size);
        Ok(())
    }

    fn ensure_bind_group(&mut self) {
        if self.bind_group.is_some() {
            return;
        }

        self.bind_group = Some(
            self.device().create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("augur_preview_bind_group"),
                layout: &self.bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniform_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            self.intensity_view
                                .as_ref()
                                .unwrap_or(&self.dummy_intensity_view),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(
                            self.polarity_view
                                .as_ref()
                                .unwrap_or(&self.dummy_polarity_view),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(
                            self.timesurface_view
                                .as_ref()
                                .unwrap_or(&self.dummy_timesurface_view),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&self.lut_view),
                    },
                ],
            }),
        );
    }

    fn upload_payload(&self, payload: PackedPreviewPayload<'_>) {
        let queue = self.queue();
        match payload {
            PackedPreviewPayload::Intensity(data) => {
                if let Some(texture) = &self.intensity_texture {
                    write_texture(
                        queue,
                        texture,
                        data.size,
                        bytemuck::cast_slice(data.values),
                        2,
                    );
                }
            }
            PackedPreviewPayload::Polarity { size } => {
                if let Some(texture) = &self.polarity_texture {
                    write_texture(
                        queue,
                        texture,
                        size,
                        bytemuck::cast_slice(&self.polarity_payload),
                        4,
                    );
                }
            }
        }
    }
}

impl Drop for WgpuPreviewRenderer {
    fn drop(&mut self) {
        self.free_display_texture();
    }
}

fn mode_id_and_colormap(mode: PreviewMode) -> (u32, u32) {
    match mode {
        PreviewMode::Intensity(colormap) => (MODE_INTENSITY, colormap.index()),
        PreviewMode::RedBlue => (MODE_RED_BLUE, Colormap::Grays.index()),
        PreviewMode::SignedCount => (MODE_SIGNED_COUNT, Colormap::BlueWhiteRed.index()),
        PreviewMode::TimeSurface => (MODE_TIME_SURFACE, Colormap::Grays.index()),
    }
}

/// Compute clamped display range parameters from preview display settings.
/// Returns `(display_min, inverse_range, gamma)` ready for shader uniforms.
fn display_range_params(settings: PreviewDisplaySettings) -> (f32, f32, f32) {
    let display_min = settings
        .display_min
        .min(settings.display_max.saturating_sub(1));
    let display_max = settings.display_max.max(display_min.saturating_add(1));
    let range = f32::from(display_max.saturating_sub(display_min).max(1));
    (
        f32::from(display_min),
        1.0 / range.max(1.0),
        settings.gamma.max(0.01),
    )
}

fn preview_uniforms(
    mode: PreviewMode,
    size: [usize; 2],
    settings: PreviewDisplaySettings,
    time_surface_tau_us: u64,
) -> PreviewUniforms {
    let (display_min, inverse_range, gamma) = display_range_params(settings);
    let (mode_id, colormap_row) = mode_id_and_colormap(mode);
    PreviewUniforms {
        mode: mode_id,
        colormap_row,
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        display_min,
        inverse_range,
        gamma,
        time_surface_tau_us: time_surface_tau_us.max(1) as f32,
        time_surface_frame_end_tick: 0,
        time_surface_tick_us: 1,
        _pad0: 0,
        _pad1: 0,
    }
}

fn count_preview_uniforms(
    mode: PreviewMode,
    size: [usize; 2],
    settings: PreviewDisplaySettings,
    event_count: usize,
    dispatch_width: u32,
) -> CountPreviewUniforms {
    let (display_min, inverse_range, gamma) = display_range_params(settings);
    let (mode_id, colormap_row) = mode_id_and_colormap(mode);
    CountPreviewUniforms {
        mode: mode_id,
        colormap_row,
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        display_min,
        inverse_range,
        gamma,
        event_count: event_count.min(u32::MAX as usize) as u32,
        dispatch_width,
        _pad0: 0,
        _pad1: 0,
        _pad2: 0,
    }
}

fn time_surface_accumulate_uniforms(
    size: [usize; 2],
    event_count: usize,
    dispatch_width: u32,
) -> TimeSurfaceAccumulateUniforms {
    TimeSurfaceAccumulateUniforms {
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        event_count: event_count.min(u32::MAX as usize) as u32,
        dispatch_width,
    }
}

fn time_surface_render_uniforms(
    size: [usize; 2],
    settings: PreviewDisplaySettings,
    frame_end_us: u64,
    time_surface_tau_us: u64,
) -> TimeSurfaceRenderUniforms {
    let (display_min, inverse_range, gamma) = display_range_params(settings);
    TimeSurfaceRenderUniforms {
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        colormap_row: Colormap::Grays.index(),
        _pad0: 0,
        display_min,
        inverse_range,
        gamma,
        time_surface_tau_us: time_surface_tau_us.max(1) as f32,
        frame_end_tick: encode_time_surface_tick(frame_end_us),
        tick_period_us: TIME_SURFACE_TICK_US as u32,
        _pad1: 0,
        _pad2: 0,
    }
}

fn time_surface_histogram_uniforms(
    size: [usize; 2],
    frame_end_us: u64,
    time_surface_tau_us: u64,
    sample_stride: usize,
    dispatch_width: u32,
) -> TimeSurfaceHistogramUniforms {
    TimeSurfaceHistogramUniforms {
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        histogram_bin_count: TIME_SURFACE_BINS as u32,
        sample_stride: sample_stride.max(1).min(u32::MAX as usize) as u32,
        frame_end_tick: encode_time_surface_tick(frame_end_us),
        tick_period_us: TIME_SURFACE_TICK_US as u32,
        dispatch_width,
        _pad1: 0,
        time_surface_tau_us: time_surface_tau_us.max(1) as f32,
        _pad2: 0,
        _pad3: 0,
        _pad4: 0,
    }
}

fn storage_uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_buffer_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    read_only: bool,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn create_storage_buffer(
    device: &wgpu::Device,
    size: u64,
    label: &str,
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: size.max(std::mem::size_of::<u32>() as u64),
        usage,
        mapped_at_creation: false,
    })
}

fn pack_count_event(event: &augur_core::pipeline::CdEvent) -> u32 {
    u32::from(event.x) | (u32::from(event.y) << 16) | (u32::from(event.polarity) << 31)
}

fn pack_time_surface_event(event: &augur_core::pipeline::CdEvent) -> [u32; 2] {
    [
        u32::from(event.x) | (u32::from(event.y) << 16),
        encode_time_surface_tick(event.timestamp),
    ]
}

/// A compute dispatch laid out so no dimension exceeds the device's limit.
///
/// One item per invocation is the natural shape for every compute pass here —
/// one per event, one per pixel — but a dispatch dimension is capped
/// (`max_compute_workgroups_per_dimension`, 65535 in the guaranteed floor and
/// on every desktop backend). At the 64-wide workgroup this file uses, a flat
/// `(n, 1, 1)` dispatch therefore dies above 4_194_240 items — and it dies as a
/// *validation panic*, not an error, taking the process with it.
///
/// A dense frame can reach that limit: 4_952_000 events ask for 77_375
/// workgroups.
///
/// So the grid grows into `y` once `x` is full, and the shader recovers the
/// linear index as `gid.x + gid.y * span`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DispatchGrid {
    x: u32,
    y: u32,
    /// Invocations covered by one full row of `x` — the shader's `y` stride.
    span: u32,
}

impl DispatchGrid {
    /// `max_per_dimension` comes from the live device rather than a constant:
    /// the limit is a property of the adapter, and a machine that allows more
    /// should not be forced into a taller grid than it needs.
    fn for_items(items: u32, max_per_dimension: u32) -> Self {
        let groups = items.max(1).div_ceil(COUNT_WORKGROUP_SIZE);
        let max = max_per_dimension.max(1);
        let x = groups.min(max);
        // `y` cannot overflow the same limit in turn: `items` is a `u32`, so
        // `groups` is at most 2^26, and dividing that by a limit of 65535
        // leaves at most 1024 rows.
        let y = groups.div_ceil(x);
        Self {
            x,
            y,
            span: x.saturating_mul(COUNT_WORKGROUP_SIZE),
        }
    }
}

fn map_buffer_sync(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    byte_len: u64,
    label: &str,
) -> Result<Vec<u32>, String> {
    let slice = buffer.slice(..byte_len);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| format!("{label} readback callback failed"))?
        .map_err(|err| format!("{label} readback failed: {err}"))?;

    let mapped = slice.get_mapped_range();
    let values: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&mapped).to_vec();
    drop(mapped);
    buffer.unmap();
    Ok(values)
}

fn read_histogram_buffer(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    len: usize,
) -> Result<Vec<u64>, String> {
    let values = map_buffer_sync(
        device,
        buffer,
        (len * std::mem::size_of::<u32>()) as u64,
        "histogram",
    )?;
    let mut histogram: Vec<u64> = values.into_iter().take(len).map(u64::from).collect();
    let trimmed_len = histogram
        .iter()
        .rposition(|&count| count != 0)
        .map(|index| index + 1)
        .unwrap_or(1);
    histogram.truncate(trimmed_len);
    Ok(histogram)
}

fn create_dummy_texture(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("augur_preview_dummy"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_source_texture(
    device: &wgpu::Device,
    size: [usize; 2],
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size[0].max(1) as u32,
            height: size[1].max(1) as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_lut_texture(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::Texture {
    let mut bytes = Vec::with_capacity(256 * Colormap::ALL.len() * 4);
    for colormap in Colormap::ALL {
        for color in colormap.table() {
            bytes.extend_from_slice(&color.to_array());
        }
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("augur_preview_lut"),
        size: wgpu::Extent3d {
            width: 256,
            height: Colormap::ALL.len() as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    write_texture(queue, &texture, [256, Colormap::ALL.len()], &bytes, 4);
    texture
}

fn write_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    size: [usize; 2],
    bytes: &[u8],
    bytes_per_pixel: u32,
) {
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(size[0].max(1) as u32 * bytes_per_pixel),
            rows_per_image: Some(size[1].max(1) as u32),
        },
        wgpu::Extent3d {
            width: size[0].max(1) as u32,
            height: size[1].max(1) as u32,
            depth_or_array_layers: 1,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{
        pack_time_surface_event, preview_uniforms, time_surface_histogram_uniforms, MODE_INTENSITY,
        MODE_SIGNED_COUNT,
    };
    use crate::{
        colormap::Colormap,
        preview::{
            compute_frame_histogram, encode_time_surface_tick, with_prepared_preview_frame,
            CpuPreviewImageCache, PreparedPreviewFrame, PreviewDisplaySettings, PreviewMode,
            TIME_SURFACE_BINS, TIME_SURFACE_TICK_US,
        },
    };
    use augur_core::pipeline::{CdEvent, PreviewFrame};
    use egui::Color32;

    use super::{
        wgpu, DispatchGrid, COUNT_COMPUTE_SHADER, COUNT_RENDER_SHADER, COUNT_WORKGROUP_SIZE,
        PREVIEW_SHADER, TIME_SURFACE_COMPUTE_SHADER, TIME_SURFACE_HISTOGRAM_SHADER,
        TIME_SURFACE_RENDER_SHADER,
    };

    /// The dispatch limit every desktop backend reports, and wgpu's guaranteed
    /// floor. Hard-coded here so the expectations below are readable.
    const LIMIT: u32 = 65_535;

    /// The crash, as a number. 4_952_000 events at a 64-wide workgroup is
    /// 77_375 workgroups — past the limit, and fatal as a validation panic
    /// rather than an error.
    #[test]
    fn the_dispatch_that_killed_a_protocol_run_now_fits() {
        let items = 4_952_000u32;
        let flat = items.div_ceil(COUNT_WORKGROUP_SIZE);
        assert!(
            flat > LIMIT,
            "the regression case no longer overflows: {flat}"
        );

        let grid = DispatchGrid::for_items(items, LIMIT);
        assert!(grid.x <= LIMIT && grid.y <= LIMIT, "{grid:?}");
        assert!(grid.y > 1, "the grid did not grow into y: {grid:?}");
        // Every item is still covered — this fix must not silently drop events.
        assert!(
            u64::from(grid.x) * u64::from(grid.y) * u64::from(COUNT_WORKGROUP_SIZE)
                >= u64::from(items),
            "{grid:?} does not cover {items} items"
        );
    }

    /// The shader recovers `i` from `gid.x + gid.y * span`. If `span` and the
    /// grid ever disagree, the preview silently reads the wrong events — so the
    /// mapping is checked here rather than trusted.
    #[test]
    fn the_grid_and_its_span_address_every_item_exactly_once() {
        for items in [1u32, 63, 64, 65, 4096, 4_194_240, 4_194_241, 9_000_000] {
            let grid = DispatchGrid::for_items(items, LIMIT);
            assert_eq!(grid.span, grid.x * COUNT_WORKGROUP_SIZE, "items={items}");
            let mut highest = 0u64;
            for y in 0..u64::from(grid.y) {
                for x in 0..u64::from(grid.span) {
                    highest = highest.max(x + y * u64::from(grid.span));
                }
            }
            assert!(
                highest + 1 >= u64::from(items),
                "items={items}: the grid reaches {highest}, short of {items}"
            );
        }
    }

    /// A single row stays one-dimensional, so the ordinary case is unchanged.
    #[test]
    fn a_small_dispatch_is_still_flat() {
        let grid = DispatchGrid::for_items(1_000, LIMIT);
        assert_eq!(grid.y, 1);
        assert_eq!(grid.x, 1_000u32.div_ceil(COUNT_WORKGROUP_SIZE));
    }

    /// A device to run the shaders on, or `None` where there is none. Callers
    /// skip rather than fail: a machine without a GPU cannot answer the
    /// question, which is different from answering it "no".
    ///
    /// **`PRIMARY` only — deliberately not `all()`, which is what the renderer
    /// itself asks for.** The GL backend is unusable from a test binary on a
    /// headless runner: `wgpu-hal`'s EGL context does an `unwrap` inside
    /// `make_current`, so a GL adapter panics somewhere in the middle of wgpu
    /// rather than returning an error this code could skip on. Excluding it
    /// means a runner with no Vulkan/Metal/DX12 adapter reports nothing at all
    /// and the test skips cleanly, which is the honest outcome.
    ///
    /// Coverage is not lost: `create_shader_module` runs naga's validation,
    /// which is what these tests are checking and is backend-independent.
    fn test_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..Default::default()
        });
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))?;
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None)).ok()
    }

    /// The linearization, executed rather than argued about. A wrong
    /// `gid.x + gid.y * span` would not crash — it would silently count the
    /// wrong events, which is worse than the panic this change removes.
    ///
    /// The grid is forced two-dimensional by handing `DispatchGrid` a limit of
    /// one workgroup per dimension, so 200 events exercise exactly the shape
    /// the bench reaches with five million.
    #[test]
    fn the_count_shader_reads_every_event_across_a_two_dimensional_grid() {
        let Some((device, queue)) = test_device() else {
            eprintln!("no wgpu device available — skipping");
            return;
        };

        const WIDTH: u32 = 8;
        const EVENTS: u32 = 200;
        // Pixel `p` receives every event whose index is `p mod 8`, and those
        // all share one polarity because the stride is even — so the expected
        // on/off split is exact rather than statistical.
        let packed: Vec<u32> = (0..EVENTS)
            .map(|i| {
                // Packed as the shader unpacks it: x in the low 16 bits, y in
                // the next 15 (always row 0 here), polarity in the top bit.
                let x = i % WIDTH;
                let polarity = i % 2;
                x | (polarity << 31)
            })
            .collect();

        let grid = DispatchGrid::for_items(EVENTS, 1);
        assert!(grid.y > 1, "the test did not force a 2D grid: {grid:?}");

        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("count_compute"),
            source: wgpu::ShaderSource::Wgsl(COUNT_COMPUTE_SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                super::storage_uniform_entry(0, wgpu::ShaderStages::COMPUTE),
                super::storage_buffer_entry(1, wgpu::ShaderStages::COMPUTE, true),
                super::storage_buffer_entry(2, wgpu::ShaderStages::COMPUTE, false),
                super::storage_buffer_entry(3, wgpu::ShaderStages::COMPUTE, false),
                super::storage_buffer_entry(4, wgpu::ShaderStages::COMPUTE, false),
                super::storage_buffer_entry(5, wgpu::ShaderStages::COMPUTE, false),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: None,
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: "cs_accumulate",
        });

        let counts_bytes = (WIDTH as u64) * 4;
        let storage = |size: u64, label: &str| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let events_buffer = storage((packed.len() * 4) as u64, "events");
        let total = storage(counts_bytes, "total");
        let on = storage(counts_bytes, "on");
        let off = storage(counts_bytes, "off");
        let histogram = storage((super::PREVIEW_HISTOGRAM_BINS * 4) as u64, "histogram");
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<super::CountPreviewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: counts_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        queue.write_buffer(&events_buffer, 0, bytemuck::cast_slice(&packed));
        queue.write_buffer(
            &uniform,
            0,
            bytemuck::bytes_of(&super::count_preview_uniforms(
                PreviewMode::Intensity(Colormap::Green),
                [WIDTH as usize, 1],
                PreviewDisplaySettings::default(),
                EVENTS as usize,
                grid.span,
            )),
        );

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: events_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: total.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: on.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: off.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: histogram.as_entire_binding(),
                },
            ],
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.clear_buffer(&total, 0, None);
        encoder.clear_buffer(&on, 0, None);
        encoder.clear_buffer(&off, 0, None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(grid.x, grid.y, 1);
        }
        encoder.copy_buffer_to_buffer(&total, 0, &readback, 0, counts_bytes);
        queue.submit(Some(encoder.finish()));

        let totals =
            super::map_buffer_sync(&device, &readback, counts_bytes, "total").expect("readback");
        assert_eq!(
            totals.iter().sum::<u32>(),
            EVENTS,
            "events were lost or double-counted: {totals:?}"
        );
        assert!(
            totals.iter().all(|count| *count == EVENTS / WIDTH),
            "events were not spread as written: {totals:?}"
        );
    }

    /// WGSL is compiled by the driver at pipeline creation, so a mistake in a
    /// shader is a *runtime* fault on the bench, not a build error here. These
    /// are the edits this change made, so they are validated against a real
    /// device — skipped, not failed, where no adapter exists.
    #[test]
    fn every_shader_still_compiles_on_a_real_device() {
        let Some((device, _queue)) = test_device() else {
            eprintln!("no wgpu device available — skipping shader validation");
            return;
        };

        for (label, source) in [
            ("preview", PREVIEW_SHADER),
            ("count_compute", COUNT_COMPUTE_SHADER),
            ("count_render", COUNT_RENDER_SHADER),
            ("time_surface_compute", TIME_SURFACE_COMPUTE_SHADER),
            ("time_surface_render", TIME_SURFACE_RENDER_SHADER),
            ("time_surface_histogram", TIME_SURFACE_HISTOGRAM_SHADER),
        ] {
            device.push_error_scope(wgpu::ErrorFilter::Validation);
            let _module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            if let Some(error) = pollster::block_on(device.pop_error_scope()) {
                panic!("{label} shader failed validation: {error}");
            }
        }
    }

    fn frame() -> PreviewFrame {
        PreviewFrame {
            width: 2,
            height: 1,
            pixels: vec![4, 6],
            pixels_on: vec![3, 2],
            pixels_off: vec![1, 4],
            cached_total_histogram: Vec::new(),
            cached_signed_histogram: Vec::new(),
            on_count: 5,
            off_count: 5,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 1,
        }
    }

    fn normalize_value(value: u16, settings: PreviewDisplaySettings) -> f32 {
        let display_min = settings
            .display_min
            .min(settings.display_max.saturating_sub(1));
        let display_max = settings.display_max.max(display_min.saturating_add(1));
        let range = f32::from(display_max.saturating_sub(display_min).max(1));
        let normalized = (f32::from(value.saturating_sub(display_min)) / range).clamp(0.0, 1.0);
        normalized.powf(settings.gamma.max(0.01))
    }

    fn lut_color(colormap: Colormap, value: f32) -> Color32 {
        colormap.table()[(value * 255.0).round().clamp(0.0, 255.0) as usize]
    }

    fn shader_reference_pixel(
        prepared: PreparedPreviewFrame<'_>,
        mode: PreviewMode,
        settings: PreviewDisplaySettings,
        index: usize,
    ) -> Color32 {
        match (prepared, mode) {
            (
                PreparedPreviewFrame::IntensityR16 { values, .. },
                PreviewMode::Intensity(colormap),
            ) => lut_color(colormap, normalize_value(values[index], settings)),
            (PreparedPreviewFrame::PolarityRg16 { on, off, total, .. }, PreviewMode::RedBlue) => {
                let total = total[index];
                if total == 0 {
                    Color32::BLACK
                } else {
                    let brightness = (normalize_value(total, settings) * 255.0)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let scale = |c: u8| {
                        ((c as f32) * (brightness as f32 / 255.0))
                            .round()
                            .clamp(0.0, 255.0) as u8
                    };
                    let mix = |a: [u8; 3], b: [u8; 3]| -> [u8; 3] {
                        [
                            ((a[0] as u16 + b[0] as u16) / 2) as u8,
                            ((a[1] as u16 + b[1] as u16) / 2) as u8,
                            ((a[2] as u16 + b[2] as u16) / 2) as u8,
                        ]
                    };
                    let on_tint = crate::theme::POLARITY_ON_RGB;
                    let off_tint = crate::theme::POLARITY_OFF_RGB;
                    match on[index].cmp(&off[index]) {
                        std::cmp::Ordering::Greater => Color32::from_rgb(
                            scale(on_tint[0]),
                            scale(on_tint[1]),
                            scale(on_tint[2]),
                        ),
                        std::cmp::Ordering::Less => Color32::from_rgb(
                            scale(off_tint[0]),
                            scale(off_tint[1]),
                            scale(off_tint[2]),
                        ),
                        std::cmp::Ordering::Equal => {
                            let m = mix(on_tint, off_tint);
                            Color32::from_rgb(scale(m[0]), scale(m[1]), scale(m[2]))
                        }
                    }
                }
            }
            (
                PreparedPreviewFrame::PolarityRg16 { on, off, total, .. },
                PreviewMode::SignedCount,
            ) => {
                if total[index] == 0 {
                    Color32::BLACK
                } else {
                    let diff = on[index].abs_diff(off[index]);
                    let magnitude = normalize_value(diff, settings);
                    let signed_t = match on[index].cmp(&off[index]) {
                        std::cmp::Ordering::Greater => 0.5 + 0.5 * magnitude,
                        std::cmp::Ordering::Less => 0.5 - 0.5 * magnitude,
                        std::cmp::Ordering::Equal => 0.5,
                    };
                    lut_color(Colormap::BlueWhiteRed, signed_t)
                }
            }
            _ => panic!("unexpected payload/mode combination"),
        }
    }

    #[test]
    fn polarity_shader_literals_match_theme_constants() {
        // The WGSL fragment shaders hardcode the polarity tints as normalized
        // float literals. They must stay byte-identical to the CPU-path theme
        // constants, or the GPU and CPU previews would render different colours.
        fn wgsl_rgb(rgb: [u8; 3]) -> String {
            format!(
                "vec3<f32>({:.4}, {:.4}, {:.4})",
                f32::from(rgb[0]) / 255.0,
                f32::from(rgb[1]) / 255.0,
                f32::from(rgb[2]) / 255.0,
            )
        }
        let on = wgsl_rgb(crate::theme::POLARITY_ON_RGB);
        let off = wgsl_rgb(crate::theme::POLARITY_OFF_RGB);
        for shader in [super::PREVIEW_SHADER, super::COUNT_RENDER_SHADER] {
            assert!(
                shader.contains(&on),
                "shader missing polarity-on literal {on}"
            );
            assert!(
                shader.contains(&off),
                "shader missing polarity-off literal {off}"
            );
        }
    }

    #[test]
    fn preview_uniforms_use_mode_specific_rows() {
        let intensity = preview_uniforms(
            PreviewMode::Intensity(Colormap::Green),
            [2, 1],
            PreviewDisplaySettings::default(),
            30_000,
        );
        assert_eq!(intensity.mode, MODE_INTENSITY);
        assert_eq!(intensity.colormap_row, Colormap::Green.index());

        let signed = preview_uniforms(
            PreviewMode::SignedCount,
            [2, 1],
            PreviewDisplaySettings::default(),
            30_000,
        );
        assert_eq!(signed.mode, MODE_SIGNED_COUNT);
        assert_eq!(signed.colormap_row, Colormap::BlueWhiteRed.index());
    }

    #[test]
    fn pack_time_surface_event_encodes_sensor_coordinates_and_tick() {
        let event = CdEvent {
            x: 13,
            y: 29,
            timestamp: 12_345,
            polarity: true,
        };

        let packed = pack_time_surface_event(&event);
        assert_eq!(packed[0], u32::from(event.x) | (u32::from(event.y) << 16));
        assert_eq!(packed[1], encode_time_surface_tick(event.timestamp));
    }

    #[test]
    fn time_surface_histogram_uniforms_encode_sampling_and_tau() {
        let uniforms = time_surface_histogram_uniforms([320, 240], 64_000, 30_000, 4, 1_024);

        assert_eq!(uniforms.width, 320);
        assert_eq!(uniforms.height, 240);
        assert_eq!(uniforms.histogram_bin_count, TIME_SURFACE_BINS as u32);
        assert_eq!(uniforms.sample_stride, 4);
        assert_eq!(uniforms.frame_end_tick, encode_time_surface_tick(64_000));
        assert_eq!(uniforms.tick_period_us, TIME_SURFACE_TICK_US as u32);
        assert_eq!(uniforms.time_surface_tau_us, 30_000.0);
    }

    #[test]
    fn polarity_payload_packs_on_and_off_counts() {
        let frame = frame();
        let mut payload: Vec<[u16; 2]> = Vec::new();
        let mut size = [0, 0];
        with_prepared_preview_frame(&frame, PreviewMode::SignedCount, 30_000, |prepared| {
            if let crate::preview::PreparedPreviewFrame::PolarityRg16 {
                size: s, on, off, ..
            } = prepared
            {
                size = s;
                payload.resize(s[0] * s[1], [0, 0]);
                for ((packed, &on), &off) in payload.iter_mut().zip(on).zip(off) {
                    *packed = [on, off];
                }
            }
        });

        assert_eq!(size, [2, 1]);
        assert_eq!(payload, vec![[3, 1], [2, 4]]);
    }

    #[test]
    fn cpu_reference_image_matches_histogram_mode_expectations() {
        let frame = frame();
        let settings = PreviewDisplaySettings {
            display_min: 0,
            display_max: 6,
            gamma: 1.0,
        };
        let image =
            with_prepared_preview_frame(&frame, PreviewMode::SignedCount, 30_000, |prepared| {
                let mut cache = CpuPreviewImageCache::default();
                cache
                    .render_prepared(prepared, settings, PreviewMode::SignedCount)
                    .clone()
            });
        let histogram = compute_frame_histogram(&frame, PreviewMode::SignedCount, 30_000);

        assert_eq!(image.size, [2, 1]);
        assert_eq!(histogram, vec![0, 0, 2]);
    }

    #[test]
    fn shader_reference_matches_cpu_reference_for_small_frame_modes() {
        let frame = frame();
        let settings = PreviewDisplaySettings {
            display_min: 0,
            display_max: 6,
            gamma: 1.0,
        };

        for mode in [
            PreviewMode::Intensity(Colormap::Green),
            PreviewMode::RedBlue,
            PreviewMode::SignedCount,
        ] {
            let cpu = with_prepared_preview_frame(&frame, mode, 30_000, |prepared| {
                let mut cache = CpuPreviewImageCache::default();
                cache.render_prepared(prepared, settings, mode).clone()
            });
            let reference = with_prepared_preview_frame(&frame, mode, 30_000, |prepared| {
                let size = prepared.size();
                (0..size[0] * size[1])
                    .map(|index| shader_reference_pixel(prepared, mode, settings, index))
                    .collect::<Vec<_>>()
            });

            assert_eq!(cpu.pixels, reference, "mode {mode:?}");
        }
    }
}
