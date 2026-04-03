use std::sync::Arc;
use std::time::Instant;

use bytemuck::{Pod, Zeroable};
use egui::{TextureHandle, TextureOptions};
use egui_wgpu::wgpu;

use crate::{
    colormap::Colormap,
    preview::{CpuPreviewImageCache, PreparedPreviewFrame, PreviewDisplaySettings, PreviewMode},
    preview_perf::PreviewPerfStats,
};

const MODE_INTENSITY: u32 = 0;
const MODE_RED_BLUE: u32 = 1;
const MODE_SIGNED_COUNT: u32 = 2;
const MODE_TIME_SURFACE: u32 = 3;

const PREVIEW_SHADER: &str = r#"
struct PreviewUniforms {
    mode: u32,
    colormap_row: u32,
    width: u32,
    height: u32,
    display_min: f32,
    inverse_range: f32,
    gamma: f32,
    _pad: f32,
};

@group(0) @binding(0)
var<uniform> uniforms: PreviewUniforms;
@group(0) @binding(1)
var intensity_tex: texture_2d<u32>;
@group(0) @binding(2)
var polarity_tex: texture_2d<u32>;
@group(0) @binding(3)
var timesurface_tex: texture_2d<f32>;
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
        if on > off {
            return vec4<f32>(brightness, 0.0, 0.0, 1.0);
        }
        if off > on {
            return vec4<f32>(0.0, 0.0, brightness, 1.0);
        }
        return vec4<f32>(brightness, 0.0, brightness, 1.0);
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

    let decay = textureLoad(timesurface_tex, coord, 0).r;
    let decay_value = u32(round(decay * 255.0));
    return vec4<f32>(lut_color(uniforms.colormap_row, normalize_value(decay_value)), 1.0);
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
        ctx: &egui::Context,
        prepared: PreparedPreviewFrame<'_>,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        match self {
            Self::Cpu(renderer) => renderer.render(ctx, prepared, settings, mode, perf),
            Self::Wgpu(renderer) => renderer.render(prepared, settings, mode, perf),
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
        ctx: &egui::Context,
        prepared: PreparedPreviewFrame<'_>,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        let render_started = Instant::now();
        let image = self.cache.render_prepared(prepared, settings, mode).clone();
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
}

#[derive(Debug, Clone, Copy)]
struct IntensityR16<'a> {
    size: [usize; 2],
    values: &'a [u16],
}

#[derive(Debug, Default, Clone)]
struct PolarityRg16 {
    size: [usize; 2],
    values: Vec<[u16; 2]>,
}

#[derive(Debug, Clone, Copy)]
struct TimeSurfaceR8<'a> {
    size: [usize; 2],
    values: &'a [u8],
}

#[derive(Debug, Clone, Copy)]
enum PackedPreviewPayload<'a> {
    Intensity(IntensityR16<'a>),
    Polarity { size: [usize; 2] },
    TimeSurface(TimeSurfaceR8<'a>),
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
    _pad: f32,
}

pub struct WgpuPreviewRenderer {
    render_state: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
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
    polarity_payload: PolarityRg16,
}

impl WgpuPreviewRenderer {
    fn new(render_state: egui_wgpu::RenderState) -> Result<Self, String> {
        let device = Arc::clone(&render_state.device);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("augur_preview_shader"),
            source: wgpu::ShaderSource::Wgsl(PREVIEW_SHADER.into()),
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
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
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

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("augur_preview_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
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

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_preview_uniforms"),
            size: std::mem::size_of::<PreviewUniforms>() as u64,
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
        let dummy_timesurface_texture = create_dummy_texture(&device, wgpu::TextureFormat::R8Unorm);
        let dummy_timesurface_view =
            dummy_timesurface_texture.create_view(&wgpu::TextureViewDescriptor::default());

        Ok(Self {
            render_state,
            pipeline,
            bind_group_layout,
            uniform_buffer,
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
            polarity_payload: PolarityRg16::default(),
        })
    }

    fn render(
        &mut self,
        prepared: PreparedPreviewFrame<'_>,
        settings: PreviewDisplaySettings,
        mode: PreviewMode,
        perf: &mut PreviewPerfStats,
    ) -> Result<PreviewDisplayTexture, String> {
        let payload = self.pack_payload(prepared);
        let size = match payload {
            PackedPreviewPayload::Intensity(data) => data.size,
            PackedPreviewPayload::Polarity { size } => size,
            PackedPreviewPayload::TimeSurface(data) => data.size,
        };
        self.ensure_textures(size)?;
        self.ensure_bind_group();

        let device = Arc::clone(&self.render_state.device);
        let queue = Arc::clone(&self.render_state.queue);
        let submit_started = Instant::now();
        self.upload_payload(&queue, payload);
        queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&preview_uniforms(mode, size, settings)),
        );

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("augur_preview_encoder"),
        });
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("augur_preview_render_pass"),
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
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(
                0,
                self.bind_group
                    .as_ref()
                    .ok_or_else(|| "missing preview bind group".to_owned())?,
                &[],
            );
            render_pass.draw(0..3, 0..1);
        }
        queue.submit(Some(encoder.finish()));
        perf.record_upload_submit(submit_started.elapsed());

        self.display_texture_id
            .map(|id| PreviewDisplayTexture::Native { id, size })
            .ok_or_else(|| "missing preview texture id".to_owned())
    }

    fn reset(&mut self) {
        self.free_display_texture();
        self.bind_group = None;
        self.intensity_texture = None;
        self.intensity_view = None;
        self.polarity_texture = None;
        self.polarity_view = None;
        self.timesurface_texture = None;
        self.timesurface_view = None;
        self.display_texture = None;
        self.display_view = None;
        self.size = None;
        self.polarity_payload.values.clear();
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
                self.polarity_payload.size = size;
                self.polarity_payload
                    .values
                    .resize(pixel_count, [0_u16, 0_u16]);
                for ((packed, &on), &off) in
                    self.polarity_payload.values.iter_mut().zip(on).zip(off)
                {
                    *packed = [on, off];
                }
                PackedPreviewPayload::Polarity { size }
            }
            PreparedPreviewFrame::TimeSurfaceR8 { size, values } => {
                PackedPreviewPayload::TimeSurface(TimeSurfaceR8 { size, values })
            }
        }
    }

    fn ensure_textures(&mut self, size: [usize; 2]) -> Result<(), String> {
        if self.size == Some(size) && self.display_texture_id.is_some() {
            return Ok(());
        }

        let device = Arc::clone(&self.render_state.device);
        let intensity_texture = create_source_texture(
            &device,
            size,
            wgpu::TextureFormat::R16Uint,
            "augur_preview_intensity",
        );
        let polarity_texture = create_source_texture(
            &device,
            size,
            wgpu::TextureFormat::Rg16Uint,
            "augur_preview_polarity",
        );
        let timesurface_texture = create_source_texture(
            &device,
            size,
            wgpu::TextureFormat::R8Unorm,
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
                &device,
                &display_view,
                wgpu::FilterMode::Linear,
                existing,
            );
            existing
        } else {
            renderer.register_native_texture(&device, &display_view, wgpu::FilterMode::Linear)
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

        let device = Arc::clone(&self.render_state.device);
        self.bind_group = Some(
            device.create_bind_group(&wgpu::BindGroupDescriptor {
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

    fn upload_payload(&self, queue: &wgpu::Queue, payload: PackedPreviewPayload<'_>) {
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
                        bytemuck::cast_slice(&self.polarity_payload.values),
                        4,
                    );
                }
            }
            PackedPreviewPayload::TimeSurface(data) => {
                if let Some(texture) = &self.timesurface_texture {
                    write_texture(queue, texture, data.size, data.values, 1);
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

fn preview_uniforms(
    mode: PreviewMode,
    size: [usize; 2],
    settings: PreviewDisplaySettings,
) -> PreviewUniforms {
    let display_min = settings
        .display_min
        .min(settings.display_max.saturating_sub(1));
    let display_max = settings.display_max.max(display_min.saturating_add(1));
    let range = f32::from(display_max.saturating_sub(display_min).max(1));
    let (mode_id, colormap_row) = match mode {
        PreviewMode::Intensity(colormap) => (MODE_INTENSITY, colormap.index()),
        PreviewMode::RedBlue => (MODE_RED_BLUE, Colormap::Grays.index()),
        PreviewMode::SignedCount => (MODE_SIGNED_COUNT, Colormap::BlueWhiteRed.index()),
        PreviewMode::TimeSurface => (MODE_TIME_SURFACE, Colormap::Grays.index()),
    };
    PreviewUniforms {
        mode: mode_id,
        colormap_row,
        width: size[0].max(1) as u32,
        height: size[1].max(1) as u32,
        display_min: f32::from(display_min),
        inverse_range: 1.0 / range.max(1.0),
        gamma: settings.gamma.max(0.01),
        _pad: 0.0,
    }
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
    use super::{preview_uniforms, PolarityRg16, MODE_INTENSITY, MODE_SIGNED_COUNT};
    use crate::{
        colormap::Colormap,
        preview::{
            compute_frame_histogram, with_prepared_preview_frame, CpuPreviewImageCache,
            PreparedPreviewFrame, PreviewDisplaySettings, PreviewMode,
        },
    };
    use augur_core::pipeline::PreviewFrame;
    use egui::Color32;

    fn frame() -> PreviewFrame {
        PreviewFrame {
            width: 2,
            height: 1,
            pixels: vec![4, 6],
            pixels_on: vec![3, 2],
            pixels_off: vec![1, 4],
            on_count: 5,
            off_count: 5,
            events: None,
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
                    match on[index].cmp(&off[index]) {
                        std::cmp::Ordering::Greater => Color32::from_rgb(brightness, 0, 0),
                        std::cmp::Ordering::Less => Color32::from_rgb(0, 0, brightness),
                        std::cmp::Ordering::Equal => Color32::from_rgb(brightness, 0, brightness),
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
    fn preview_uniforms_use_mode_specific_rows() {
        let intensity = preview_uniforms(
            PreviewMode::Intensity(Colormap::Green),
            [2, 1],
            PreviewDisplaySettings::default(),
        );
        assert_eq!(intensity.mode, MODE_INTENSITY);
        assert_eq!(intensity.colormap_row, Colormap::Green.index());

        let signed = preview_uniforms(
            PreviewMode::SignedCount,
            [2, 1],
            PreviewDisplaySettings::default(),
        );
        assert_eq!(signed.mode, MODE_SIGNED_COUNT);
        assert_eq!(signed.colormap_row, Colormap::BlueWhiteRed.index());
    }

    #[test]
    fn polarity_payload_packs_on_and_off_counts() {
        let frame = frame();
        let mut payload = PolarityRg16 {
            size: [0, 0],
            values: Vec::new(),
        };
        with_prepared_preview_frame(&frame, PreviewMode::SignedCount, 30_000, |prepared| {
            if let crate::preview::PreparedPreviewFrame::PolarityRg16 { size, on, off, .. } =
                prepared
            {
                payload.size = size;
                payload.values.resize(size[0] * size[1], [0, 0]);
                for ((packed, &on), &off) in payload.values.iter_mut().zip(on).zip(off) {
                    *packed = [on, off];
                }
            }
        });

        assert_eq!(payload.size, [2, 1]);
        assert_eq!(payload.values, vec![[3, 1], [2, 4]]);
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
