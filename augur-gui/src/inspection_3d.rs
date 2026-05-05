use std::sync::Arc;

use egui_wgpu::wgpu;
use glam::{Mat4, Vec2, Vec3, Vec4};

use crate::{
    investigation::StableRowKey, point_cloud::PointCloudState,
    preview_renderer::PreviewDisplayTexture,
};

const CAMERA_DISTANCE_MIN: f32 = 0.25;
const CAMERA_DISTANCE_MAX: f32 = 10_000.0;
const DEFAULT_POINT_SCALE: f32 = 8.0;

const INSPECTION_3D_SHADER: &str = r#"
struct SceneUniforms {
    view_proj: mat4x4<f32>,
    viewport: vec2<f32>,
    point_scale: f32,
    _pad0: f32,
};

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) size: f32,
    @location(3) selected: f32,
};

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) selected: f32,
};

@group(0) @binding(0)
var<uniform> uniforms: SceneUniforms;

fn quad_corner(vertex_index: u32) -> vec2<f32> {
    switch vertex_index {
        case 0u {
            return vec2<f32>(-1.0, -1.0);
        }
        case 1u {
            return vec2<f32>(1.0, -1.0);
        }
        case 2u {
            return vec2<f32>(1.0, 1.0);
        }
        case 3u {
            return vec2<f32>(-1.0, -1.0);
        }
        case 4u {
            return vec2<f32>(1.0, 1.0);
        }
        default {
            return vec2<f32>(-1.0, 1.0);
        }
    }
}

@vertex
fn vs_main(vertex: VertexInput, @builtin(vertex_index) vertex_index: u32) -> VsOut {
    let clip = uniforms.view_proj * vec4<f32>(vertex.position, 1.0);
    let corner = quad_corner(vertex_index);
    let clip_w = max(abs(clip.w), 0.0001);
    let radius_px = max(1.5, vertex.size * uniforms.point_scale / clip_w);
    let offset_ndc = corner * radius_px * vec2<f32>(
        2.0 / max(uniforms.viewport.x, 1.0),
        2.0 / max(uniforms.viewport.y, 1.0)
    );

    var out: VsOut;
    out.position = vec4<f32>(clip.xy + offset_ndc * clip.w, clip.z, clip.w);
    out.local = corner;
    out.color = vertex.color;
    out.selected = vertex.selected;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.local);
    if r > 1.05 {
        discard;
    }

    let disc = smoothstep(1.0, 0.55, r);
    let glow = (1.0 - smoothstep(0.55, 1.05, r)) * (0.12 + in.selected * 0.55);
    let highlight = mix(in.color.rgb, vec3<f32>(1.0, 0.96, 0.78), in.selected * 0.8);
    let rgb = highlight * (0.55 + 0.45 * disc) + glow * 0.35;
    let alpha = in.color.a * max(disc, glow);
    return vec4<f32>(rgb, alpha);
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SceneUniforms {
    view_proj: [[f32; 4]; 4],
    viewport: [f32; 2],
    point_scale: f32,
    _pad0: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PointInstanceRaw {
    position: [f32; 3],
    color: [f32; 4],
    size: f32,
    selected: f32,
}

#[derive(Debug, Clone)]
pub struct Investigation3dPoint {
    pub position: [f32; 3],
    pub color: [u8; 4],
    pub size: f32,
    pub item_key: Option<StableRowKey>,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Investigation3dLayer {
    pub id: String,
    pub title: String,
    pub visible: bool,
    pub points: Vec<Investigation3dPoint>,
}

#[derive(Debug, Clone)]
pub struct Investigation3dFocusVolume {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub label: String,
    pub color: [u8; 4],
}

#[derive(Debug, Clone, Default)]
pub struct Investigation3dScene {
    pub layers: Vec<Investigation3dLayer>,
    pub focus_volume: Option<Investigation3dFocusVolume>,
}

impl Investigation3dScene {
    pub fn is_empty(&self) -> bool {
        self.layers
            .iter()
            .filter(|layer| layer.visible)
            .all(|layer| layer.points.is_empty())
    }

    pub fn visible_point_count(&self) -> usize {
        self.layers
            .iter()
            .filter(|layer| layer.visible)
            .map(|layer| layer.points.len())
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisPreset {
    Xy,
    Xz,
    Yz,
    Isometric,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Investigation3dState {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: [f32; 3],
    pub display_strip_open: bool,
    pub clip_enabled: bool,
    pub clip_half_extent: f32,
    pub point_scale: f32,
    pub preset: AxisPreset,
    /// Set to true after the first auto-fit. Prevents repeated auto-fits.
    pub auto_fitted: bool,
}

impl Default for Investigation3dState {
    fn default() -> Self {
        Self {
            yaw: 0.65,
            pitch: 0.52,
            distance: 12.0,
            target: [0.0, 0.0, 0.0],
            display_strip_open: true,
            clip_enabled: false,
            clip_half_extent: 240.0,
            point_scale: DEFAULT_POINT_SCALE,
            preset: AxisPreset::Isometric,
            auto_fitted: false,
        }
    }
}

impl Investigation3dState {
    pub fn set_axis_preset(&mut self, preset: AxisPreset) {
        self.preset = preset;
        match preset {
            AxisPreset::Xy => {
                self.yaw = 0.0;
                self.pitch = 0.0;
            }
            AxisPreset::Xz => {
                self.yaw = 0.0;
                self.pitch = -std::f32::consts::FRAC_PI_2 + 0.001;
            }
            AxisPreset::Yz => {
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = 0.0;
            }
            AxisPreset::Isometric => {
                self.yaw = 0.65;
                self.pitch = 0.52;
            }
        }
    }

    pub fn reset_camera(&mut self) {
        let auto_fitted = self.auto_fitted;
        *self = Self::default();
        self.auto_fitted = auto_fitted;
    }

    pub fn focus_on(&mut self, target: [f32; 3]) {
        self.target = target;
        self.distance = self.distance.clamp(4.0, 24.0);
    }

    /// Auto-fit the camera to encompass all points in the scene.
    pub fn fit_to_scene(&mut self, scene: &Investigation3dScene) {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        let mut count = 0usize;

        for layer in scene.layers.iter().filter(|l| l.visible) {
            for point in &layer.points {
                for i in 0..3 {
                    min[i] = min[i].min(point.position[i]);
                    max[i] = max[i].max(point.position[i]);
                }
                count += 1;
            }
        }

        if count == 0 {
            return;
        }

        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let extent = [
            (max[0] - min[0]).abs(),
            (max[1] - min[1]).abs(),
            (max[2] - min[2]).abs(),
        ];
        let max_extent = extent[0].max(extent[1]).max(extent[2]).max(1.0);
        let half_extent = (max_extent * 0.5).max(1.0);
        let fov_y = 45.0_f32.to_radians();

        self.target = center;
        self.distance = (half_extent / (fov_y * 0.5).tan() * 1.15)
            .clamp(CAMERA_DISTANCE_MIN, CAMERA_DISTANCE_MAX);
    }

    fn target_vec3(&self) -> Vec3 {
        Vec3::from_array(self.target)
    }

    fn eye(&self) -> Vec3 {
        let target = self.target_vec3();
        let forward = Vec3::new(
            self.yaw.cos() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.sin() * self.pitch.cos(),
        );
        target + forward.normalize_or_zero() * self.distance
    }

    fn view_matrix(&self) -> Mat4 {
        Mat4::look_at_rh(self.eye(), self.target_vec3(), Vec3::Y)
    }

    fn projection_matrix(&self, size: egui::Vec2) -> Mat4 {
        let aspect = (size.x / size.y.max(1.0)).max(0.001);
        Mat4::perspective_rh_gl(45.0_f32.to_radians(), aspect, 0.05, 4096.0)
    }

    fn view_projection(&self, size: egui::Vec2) -> Mat4 {
        self.projection_matrix(size) * self.view_matrix()
    }

    fn camera_basis(&self) -> (Vec3, Vec3, Vec3) {
        let eye = self.eye();
        let target = self.target_vec3();
        let forward = (target - eye).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        (forward, right, up)
    }

    fn apply_drag(&mut self, delta: Vec2, pan: bool) {
        if pan {
            let (_, right, up) = self.camera_basis();
            let scale = self.distance * 0.0025;
            let target = self.target_vec3() + (-delta.x * scale) * right + (delta.y * scale) * up;
            self.target = target.to_array();
            return;
        }

        self.yaw -= delta.x * 0.01;
        self.pitch = (self.pitch + delta.y * 0.01).clamp(-1.52, 1.52);
    }

    fn apply_zoom(&mut self, scroll_delta: f32) {
        if scroll_delta.abs() <= f32::EPSILON {
            return;
        }
        self.distance = (self.distance * (1.0 - scroll_delta * 0.0015))
            .clamp(CAMERA_DISTANCE_MIN, CAMERA_DISTANCE_MAX);
    }
}

#[derive(Debug, Clone, Default)]
pub struct Investigation3dOutput {
    pub selected: Option<StableRowKey>,
    pub hovered: Option<StableRowKey>,
    pub focus_target: Option<[f32; 3]>,
}

#[derive(Debug, Clone)]
struct PreparedPoint {
    raw: PointInstanceRaw,
    item_key: Option<StableRowKey>,
    label: String,
    position: Vec3,
}

type RenderTextureOutput = (PreviewDisplayTexture, Arc<Vec<PreparedPoint>>);
type RenderTextureResult = Result<Option<RenderTextureOutput>, String>;

#[derive(Debug, Clone, Copy)]
struct RawHistoryFooterStatus {
    requested_ms: f32,
    retained_ms: Option<f32>,
    sample_count: usize,
    point_limit: usize,
}

impl RawHistoryFooterStatus {
    fn from_state(raw_history: &PointCloudState) -> Self {
        let summary = raw_history.visible_summary();
        Self {
            requested_ms: raw_history.time_window_ms,
            retained_ms: summary.retained_time_span_ms,
            sample_count: summary.sampled_count,
            point_limit: raw_history.point_limit,
        }
    }
}

pub(crate) enum Investigation3dRenderer {
    Disabled,
    Wgpu(Box<WgpuInvestigation3dRenderer>),
}

impl Default for Investigation3dRenderer {
    fn default() -> Self {
        Self::Disabled
    }
}

impl Investigation3dRenderer {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        match cc.wgpu_render_state.clone() {
            Some(render_state) => {
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    WgpuInvestigation3dRenderer::new(render_state)
                })) {
                    Ok(Ok(renderer)) => Self::Wgpu(Box::new(renderer)),
                    Ok(Err(err)) => {
                        eprintln!("investigation 3D renderer disabled: {err}");
                        Self::Disabled
                    }
                    Err(_) => {
                        eprintln!(
                            "investigation 3D renderer disabled: WGPU initialization panicked"
                        );
                        Self::Disabled
                    }
                }
            }
            None => Self::Disabled,
        }
    }

    pub fn is_wgpu(&self) -> bool {
        matches!(self, Self::Wgpu(_))
    }

    fn render_texture(
        &mut self,
        scene: &Investigation3dScene,
        state: &Investigation3dState,
        size: [usize; 2],
        selected: Option<&StableRowKey>,
    ) -> RenderTextureResult {
        match self {
            Self::Disabled => Ok(None),
            Self::Wgpu(renderer) => renderer.render(scene, state, size, selected),
        }
    }
}

pub(crate) struct WgpuInvestigation3dRenderer {
    render_state: egui_wgpu::RenderState,
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    instance_buffer: Option<wgpu::Buffer>,
    instance_capacity: usize,
    display_texture: Option<wgpu::Texture>,
    display_view: Option<wgpu::TextureView>,
    depth_texture: Option<wgpu::Texture>,
    depth_view: Option<wgpu::TextureView>,
    display_texture_id: Option<egui::TextureId>,
    size: Option<[usize; 2]>,
}

impl WgpuInvestigation3dRenderer {
    fn new(render_state: egui_wgpu::RenderState) -> Result<Self, String> {
        let device = &render_state.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("augur_investigation_3d_shader"),
            source: wgpu::ShaderSource::Wgsl(INSPECTION_3D_SHADER.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("augur_investigation_3d_bind_group_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("augur_investigation_3d_pipeline_layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_investigation_3d_uniforms"),
            size: std::mem::size_of::<SceneUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("augur_investigation_3d_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("augur_investigation_3d_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<PointInstanceRaw>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: std::mem::size_of::<[f32; 3]>() as u64,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: (std::mem::size_of::<[f32; 3]>()
                                + std::mem::size_of::<[f32; 4]>())
                                as u64,
                            shader_location: 2,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32,
                            offset: (std::mem::size_of::<[f32; 3]>()
                                + std::mem::size_of::<[f32; 4]>()
                                + std::mem::size_of::<f32>())
                                as u64,
                            shader_location: 3,
                        },
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth32Float,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::LessEqual,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        Ok(Self {
            render_state,
            pipeline,
            uniform_buffer,
            bind_group,
            instance_buffer: None,
            instance_capacity: 0,
            display_texture: None,
            display_view: None,
            depth_texture: None,
            depth_view: None,
            display_texture_id: None,
            size: None,
        })
    }

    fn device(&self) -> &wgpu::Device {
        &self.render_state.device
    }

    fn queue(&self) -> &wgpu::Queue {
        &self.render_state.queue
    }

    fn ensure_target(&mut self, size: [usize; 2]) {
        if self.size == Some(size) {
            return;
        }

        let device = self.device();
        let display_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("augur_investigation_3d_display"),
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

        let depth_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("augur_investigation_3d_depth"),
            size: wgpu::Extent3d {
                width: size[0].max(1) as u32,
                height: size[1].max(1) as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth_texture.create_view(&wgpu::TextureViewDescriptor::default());

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

        self.display_texture = Some(display_texture);
        self.display_view = Some(display_view);
        self.depth_texture = Some(depth_texture);
        self.depth_view = Some(depth_view);
        self.display_texture_id = Some(display_texture_id);
        self.size = Some(size);
    }

    fn ensure_instance_capacity(&mut self, instances: usize) {
        if instances <= self.instance_capacity {
            return;
        }
        let capacity = instances.next_power_of_two().max(256);
        self.instance_capacity = capacity;
        self.instance_buffer = Some(self.device().create_buffer(&wgpu::BufferDescriptor {
            label: Some("augur_investigation_3d_instances"),
            size: (capacity * std::mem::size_of::<PointInstanceRaw>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }

    fn render(
        &mut self,
        scene: &Investigation3dScene,
        state: &Investigation3dState,
        size: [usize; 2],
        selected: Option<&StableRowKey>,
    ) -> RenderTextureResult {
        self.ensure_target(size);

        let prepared = Arc::new(prepare_scene_points(scene, state, selected));
        self.ensure_instance_capacity(prepared.len().max(1));

        let view_proj = state.view_projection(egui::vec2(size[0] as f32, size[1] as f32));
        let uniforms = SceneUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            viewport: [size[0] as f32, size[1] as f32],
            point_scale: state.point_scale,
            _pad0: 0.0,
        };
        self.queue()
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        if let Some(instance_buffer) = &self.instance_buffer {
            let raw: Vec<PointInstanceRaw> = prepared.iter().map(|point| point.raw).collect();
            self.queue()
                .write_buffer(instance_buffer, 0, bytemuck::cast_slice(raw.as_slice()));
        }

        let Some(display_view) = &self.display_view else {
            return Ok(None);
        };
        let Some(depth_view) = &self.depth_view else {
            return Ok(None);
        };

        let mut encoder = self
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("augur_investigation_3d_encoder"),
            });
        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("augur_investigation_3d_render_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: display_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.045,
                            g: 0.055,
                            b: 0.065,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            render_pass.set_pipeline(&self.pipeline);
            render_pass.set_bind_group(0, &self.bind_group, &[]);
            if let Some(instance_buffer) = &self.instance_buffer {
                render_pass.set_vertex_buffer(0, instance_buffer.slice(..));
                render_pass.draw(0..6, 0..prepared.len() as u32);
            }
        }
        self.queue().submit(Some(encoder.finish()));

        let Some(texture_id) = self.display_texture_id else {
            return Ok(None);
        };
        Ok(Some((
            PreviewDisplayTexture::Native {
                id: texture_id,
                size,
            },
            prepared,
        )))
    }
}

fn prepare_scene_points(
    scene: &Investigation3dScene,
    state: &Investigation3dState,
    selected: Option<&StableRowKey>,
) -> Vec<PreparedPoint> {
    let view = state.view_matrix();
    let mut prepared = Vec::new();

    for layer in scene.layers.iter().filter(|layer| layer.visible) {
        debug_assert!(
            !layer.id.is_empty(),
            "investigation layers should have stable ids"
        );
        for point in &layer.points {
            let position = Vec3::from_array(point.position);
            if state.clip_enabled {
                let camera_space = view * position.extend(1.0);
                let depth = -camera_space.z;
                if depth.abs() > state.clip_half_extent {
                    continue;
                }
            }

            prepared.push(PreparedPoint {
                raw: PointInstanceRaw {
                    position: point.position,
                    color: [
                        f32::from(point.color[0]) / 255.0,
                        f32::from(point.color[1]) / 255.0,
                        f32::from(point.color[2]) / 255.0,
                        f32::from(point.color[3]) / 255.0,
                    ],
                    size: point.size.max(0.75),
                    selected: if point.item_key.as_ref() == selected {
                        1.0
                    } else {
                        0.0
                    },
                },
                item_key: point.item_key.clone(),
                label: point.label.clone(),
                position,
            });
        }
    }

    prepared
}

pub fn draw_investigation_3d(
    ui: &mut egui::Ui,
    renderer: &mut Investigation3dRenderer,
    state: &mut Investigation3dState,
    scene: &Investigation3dScene,
    selected: Option<&StableRowKey>,
    raw_history: Option<&mut PointCloudState>,
    max_height: f32,
) -> Investigation3dOutput {
    let mut output = Investigation3dOutput::default();
    let scene_empty = scene.is_empty();
    let selected_focus_target = selected.and_then(|key| scene_focus_target(scene, key));
    let total_available_height = ui.available_height().min(ui.clip_rect().height());

    let toolbar_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y + 8.0;
    let display_strip_reserve = 88.0;
    let footer_height = 96.0;
    egui::ScrollArea::horizontal()
        .id_source("investigation_3d_toolbar_scroll")
        .auto_shrink([false, false])
        .max_height(toolbar_height)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button("Reset view")
                    .on_hover_text(
                        "Restore the default orbit camera, point scale, and depth-slice settings.",
                    )
                    .clicked()
                {
                    state.reset_camera();
                }
                if ui
                    .add_enabled(!scene_empty, egui::Button::new("Fit view"))
                    .on_hover_text(
                        "Frame all visible data. Double-clicking inside the viewport does the same.",
                    )
                    .clicked()
                {
                    state.fit_to_scene(scene);
                }
                if ui
                    .add_enabled(
                        selected_focus_target.is_some(),
                        egui::Button::new("Focus selected"),
                    )
                    .on_hover_text("Center the camera target on the currently selected point.")
                    .clicked()
                {
                    output.focus_target = selected_focus_target;
                }
                ui.separator();
                for (preset, label) in [
                    (AxisPreset::Xy, "XY"),
                    (AxisPreset::Xz, "XZ"),
                    (AxisPreset::Yz, "YZ"),
                    (AxisPreset::Isometric, "ISO"),
                ] {
                    let tooltip = match preset {
                        AxisPreset::Xy => "Look straight onto the sensor plane.",
                        AxisPreset::Xz => {
                            "Look along the vertical sensor axis to compare horizontal position against time depth."
                        }
                        AxisPreset::Yz => {
                            "Look along the horizontal sensor axis to compare vertical position against time depth."
                        }
                        AxisPreset::Isometric => "Return to the default angled inspection view.",
                    };
                    if ui
                        .selectable_label(state.preset == preset, label)
                        .on_hover_text(tooltip)
                        .clicked()
                    {
                        state.set_axis_preset(preset);
                    }
                }
            });
        });

    let raw_history_status = draw_3d_display_strip(ui, state, raw_history, display_strip_reserve);
    ui.separator();

    let available_w = ui.available_width().min(ui.clip_rect().width()).max(1.0);
    let layout_overhead = toolbar_height + 20.0;
    let viewport_h =
        (total_available_height - display_strip_reserve - footer_height - layout_overhead)
            .max(100.0)
            .min(max_height);
    let desired = egui::vec2(available_w, viewport_h);
    let (rect, response) = ui.allocate_exact_size(
        desired,
        egui::Sense::click_and_drag().union(egui::Sense::hover()),
    );
    response.clone().on_hover_text(
        "Drag to orbit. Shift-drag or right-drag to pan. Scroll to zoom. Double-click to fit the visible cloud.",
    );

    if response.double_clicked() && !scene_empty {
        state.fit_to_scene(scene);
    }
    if response.dragged() {
        let delta = ui.ctx().input(|input| input.pointer.delta());
        let pan = ui.ctx().input(|input| {
            input.modifiers.shift
                || input.pointer.button_down(egui::PointerButton::Secondary)
                || input.pointer.button_down(egui::PointerButton::Middle)
        });
        state.apply_drag(Vec2::new(delta.x, delta.y), pan);
    }
    if response.hovered() {
        let scroll_y = ui.ctx().input(|input| input.raw_scroll_delta.y);
        state.apply_zoom(scroll_y);
    }

    let paint_texture = renderer
        .render_texture(
            scene,
            state,
            [
                rect.width().round().max(1.0) as usize,
                rect.height().round().max(1.0) as usize,
            ],
            selected,
        )
        .ok()
        .flatten();

    // Clamp the paint rect to the UI clip rect to prevent bleeding into side panels.
    let paint_rect = rect.intersect(ui.clip_rect());

    match paint_texture {
        Some((texture, prepared)) => {
            texture.paint_at(
                ui,
                paint_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            );
            if let Some(focus_volume) = &scene.focus_volume {
                paint_focus_volume(ui, paint_rect, state, focus_volume);
            }
            let hovered = response
                .hover_pos()
                .and_then(|pos| pick_point(rect, pos, state, prepared.as_slice()));
            output.hovered = hovered.and_then(|point| point.item_key.clone());
            if let Some(point) = hovered {
                let label_rect = egui::Rect::from_min_size(
                    rect.left_top() + egui::vec2(10.0, 10.0),
                    egui::vec2((rect.width() - 20.0).max(80.0), 22.0),
                );
                ui.painter_at(paint_rect).text(
                    label_rect.left_top(),
                    egui::Align2::LEFT_TOP,
                    &point.label,
                    egui::FontId::proportional(13.0),
                    egui::Color32::WHITE,
                );
            }
            if response.clicked() {
                if let Some(point) = response
                    .interact_pointer_pos()
                    .and_then(|pos| pick_point(rect, pos, state, prepared.as_slice()))
                {
                    output.selected = point.item_key.clone();
                    output.focus_target = Some(point.position.to_array());
                }
            }
        }
        None => {
            let painter = ui.painter_at(paint_rect);
            painter.rect_filled(paint_rect, 6.0, ui.visuals().extreme_bg_color);
            let message = if renderer.is_wgpu() {
                if scene_empty {
                    "No 3D data yet.\nLoad a replay or start streaming to see raw events.\nPlugins with 3D scatter views will also appear here."
                } else {
                    "3D inspection could not prepare a render target.\nTry resizing the window or restarting the application."
                }
            } else {
                "3D inspection requires the WGPU renderer.\nRestart with AUGUR_RENDERER=wgpu or AUGUR_RENDERER=auto."
            };
            painter.text(
                paint_rect.center(),
                egui::Align2::CENTER_CENTER,
                message,
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }
    }

    ui.add_space(2.0);
    draw_3d_status_footer(ui, response.id, scene, raw_history_status, footer_height);

    output
}

fn draw_3d_display_strip(
    ui: &mut egui::Ui,
    state: &mut Investigation3dState,
    raw_history: Option<&mut PointCloudState>,
    _max_height: f32,
) -> Option<RawHistoryFooterStatus> {
    let mut raw_history = raw_history;
    if let Some(history) = raw_history.as_deref_mut() {
        history.sanitize_controls();
    }
    let mut raw_history_status = raw_history
        .as_deref()
        .map(RawHistoryFooterStatus::from_state);

    ui.push_id(ui.id().with("display_strip"), |ui| {
        draw_section_toggle(ui, &mut state.display_strip_open, "Display");
        if !state.display_strip_open {
            return;
        }

        let strip_row_height =
            ui.spacing().interact_size.y + ui.spacing().item_spacing.y + 4.0;
        egui::ScrollArea::horizontal()
            .id_source("3d_display_strip_scroll_a")
            .auto_shrink([false, false])
            .max_height(strip_row_height)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut state.clip_enabled, "Depth slice")
                        .on_hover_text(
                            "Hide points outside a camera-aligned slab so dense clouds are easier to inspect.",
                        );
                    ui.add_enabled(
                        state.clip_enabled,
                        egui::Slider::new(&mut state.clip_half_extent, 10.0..=4_000.0)
                            .text("Slice depth")
                            .logarithmic(true),
                    )
                    .on_hover_text(
                        "Thickness of the depth slice. Larger values reveal more of the cloud.",
                    );
                    ui.separator();
                    ui.add(
                        egui::Slider::new(&mut state.point_scale, 2.0..=24.0)
                            .text("Point scale")
                            .logarithmic(true),
                    )
                    .on_hover_text(
                        "Change the rendered point size without changing the underlying data.",
                    );
                });
            });

        if let Some(history) = raw_history {
            egui::ScrollArea::horizontal()
                .id_source("3d_display_strip_scroll_b")
                .auto_shrink([false, false])
                .max_height(strip_row_height)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::Slider::new(&mut history.time_window_ms, 5.0..=5_000.0)
                                .text("History [ms]")
                                .logarithmic(true),
                        )
                        .on_hover_text(
                            "Extend or shrink the raw-event time span shown in 3D. The 2D preview window stays unchanged.",
                        );
                        ui.add(
                            egui::Slider::new(&mut history.point_limit, 1_000..=100_000)
                                .text("Max points")
                                .logarithmic(true),
                        )
                        .on_hover_text(
                            "Cap the retained raw-event sample for the 3D view so dense windows stay responsive.",
                        );
                    });
                });
            raw_history_status = Some(RawHistoryFooterStatus::from_state(history));
        }
    });

    raw_history_status
}

fn draw_3d_status_footer(
    ui: &mut egui::Ui,
    response_id: egui::Id,
    scene: &Investigation3dScene,
    raw_history_status: Option<RawHistoryFooterStatus>,
    max_height: f32,
) {
    let visible_layers: Vec<_> = scene
        .layers
        .iter()
        .filter(|layer| layer.visible)
        .map(|layer| layer.title.as_str())
        .collect();
    let focus_label = scene
        .focus_volume
        .as_ref()
        .map(|focus| focus.label.as_str());

    ui.push_id(response_id.with("footer"), |ui| {
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .auto_shrink([true, true])
            .show(ui, |ui| {
                constrain_section_width(ui);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let mut first = true;
                    draw_inline_separator(ui, &mut first);
                    ui.small(format!("Layers: {}", visible_layers.len()));
                    draw_inline_separator(ui, &mut first);
                    ui.small(format!("Points: {}", scene.visible_point_count()));

                    if let Some(status) = raw_history_status {
                        draw_inline_separator(ui, &mut first);
                        ui.small(format!(
                            "History: {:.1}/{:.0} ms",
                            status.retained_ms.unwrap_or(0.0),
                            status.requested_ms
                        ));
                    }

                    if let Some(label) = focus_label {
                        draw_inline_separator(ui, &mut first);
                        ui.small(format!("Focus: {label}"));
                    }
                });

                egui::CollapsingHeader::new("View details")
                    .id_source("investigation_3d_view_details")
                    .default_open(false)
                    .show(ui, |ui| {
                        constrain_section_width(ui);
                        ui.small(format!(
                            "Layers: {}{}",
                            visible_layers.len(),
                            if visible_layers.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", visible_layers.join(", "))
                            }
                        ));
                        if let Some(status) = raw_history_status {
                            let retained_label = status.retained_ms.unwrap_or(0.0);
                            let suffix = if status.sample_count > 0
                                && retained_label + f32::EPSILON < status.requested_ms
                            {
                                " from the current buffer"
                            } else {
                                ""
                            };
                            ui.small(format!(
                                "History: requested {:.0} ms, retained {:.1} ms{}, sample {} / {} points.",
                                status.requested_ms,
                                retained_label,
                                suffix,
                                status.sample_count,
                                status.point_limit
                            ));
                        }
                        if let Some(label) = focus_label {
                            ui.small(format!("Focus volume: {label}"));
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "Orientation: sensor X points right, sensor Y points up to match the 2D preview, and older events extend deeper into the cloud.",
                                )
                                .small(),
                            )
                            .wrap(true),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "Controls: drag to orbit, Shift/right-drag to pan, scroll to zoom, double-click to fit.",
                                )
                                .small(),
                            )
                            .wrap(true),
                        );
                    });
            });
    });
}

fn draw_section_toggle(ui: &mut egui::Ui, open: &mut bool, label: &str) {
    let chevron = if *open {
        egui_phosphor::regular::CARET_DOWN
    } else {
        egui_phosphor::regular::CARET_RIGHT
    };
    let response = ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let chevron_response = ui
            .add(
                egui::Label::new(egui::RichText::new(chevron).size(10.0))
                    .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let icon_response = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(egui_phosphor::regular::SLIDERS_HORIZONTAL)
                        .size(14.0)
                        .weak(),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let label_response = ui
            .add(
                egui::Label::new(egui::RichText::new(label).size(12.0).strong())
                    .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        chevron_response.union(icon_response).union(label_response)
    });
    if response.inner.clicked() {
        *open = !*open;
    }
}

fn constrain_section_width(ui: &mut egui::Ui) -> f32 {
    let width = ui.available_width().min(ui.clip_rect().width()).max(0.0);
    ui.set_min_width(width);
    ui.set_max_width(width);
    ui.set_width(width);
    ui.style_mut().wrap = Some(true);
    width
}

fn draw_inline_separator(ui: &mut egui::Ui, first: &mut bool) {
    if !*first {
        ui.label(egui::RichText::new("\u{2022}").weak().size(10.0));
    }
    *first = false;
}

fn scene_focus_target(scene: &Investigation3dScene, key: &StableRowKey) -> Option<[f32; 3]> {
    scene
        .layers
        .iter()
        .filter(|layer| layer.visible)
        .flat_map(|layer| layer.points.iter())
        .find(|point| point.item_key.as_ref() == Some(key))
        .map(|point| point.position)
}

fn paint_focus_volume(
    ui: &egui::Ui,
    rect: egui::Rect,
    state: &Investigation3dState,
    focus_volume: &Investigation3dFocusVolume,
) {
    let corners = focus_volume_corners(focus_volume);
    let view_proj = state.view_projection(rect.size());
    let projected: Vec<_> = corners
        .iter()
        .map(|corner| project_point(rect, view_proj, Vec3::from_array(*corner)))
        .collect();
    let painter = ui.painter_at(rect.intersect(ui.clip_rect()));
    let color = egui::Color32::from_rgba_unmultiplied(
        focus_volume.color[0],
        focus_volume.color[1],
        focus_volume.color[2],
        focus_volume.color[3],
    );
    let stroke = egui::Stroke::new(1.5, color);

    for &(from, to) in &[
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ] {
        let (Some(a), Some(b)) = (projected[from], projected[to]) else {
            continue;
        };
        painter.line_segment([a, b], stroke);
    }
}

fn focus_volume_corners(focus_volume: &Investigation3dFocusVolume) -> [[f32; 3]; 8] {
    let [min_x, min_y, min_z] = focus_volume.min;
    let [max_x, max_y, max_z] = focus_volume.max;
    [
        [min_x, min_y, min_z],
        [max_x, min_y, min_z],
        [max_x, max_y, min_z],
        [min_x, max_y, min_z],
        [min_x, min_y, max_z],
        [max_x, min_y, max_z],
        [max_x, max_y, max_z],
        [min_x, max_y, max_z],
    ]
}

fn pick_point<'a>(
    rect: egui::Rect,
    pointer: egui::Pos2,
    state: &Investigation3dState,
    points: &'a [PreparedPoint],
) -> Option<&'a PreparedPoint> {
    let size = rect.size();
    let view_proj = state.view_projection(size);
    let mut best: Option<(&PreparedPoint, f32, f32)> = None;
    for point in points {
        let screen = project_point(rect, view_proj, point.position)?;
        let distance = screen.distance(pointer);
        let clip = view_proj * point.position.extend(1.0);
        let radius = (point.raw.size * state.point_scale / clip.w.abs().max(0.0001)).max(3.0);
        if distance > radius + 6.0 {
            continue;
        }

        let depth = clip.z / clip.w;
        match best {
            Some((_, best_distance, best_depth))
                if distance > best_distance && depth >= best_depth =>
            {
                continue;
            }
            _ => best = Some((point, distance, depth)),
        }
    }
    best.map(|(point, _, _)| point)
}

fn project_point(rect: egui::Rect, view_proj: Mat4, position: Vec3) -> Option<egui::Pos2> {
    let clip: Vec4 = view_proj * position.extend(1.0);
    if clip.w.abs() <= 1e-5 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    if ndc.z < -1.2 || ndc.z > 1.2 {
        return None;
    }

    Some(egui::pos2(
        rect.left() + (ndc.x + 1.0) * 0.5 * rect.width(),
        rect.top() + (1.0 - (ndc.y + 1.0) * 0.5) * rect.height(),
    ))
}
