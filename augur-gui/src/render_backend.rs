use eframe::{egui_wgpu, NativeOptions, Renderer};

use crate::app::CameraApp;

const APP_TITLE: &str = "AugurRS — EVK4 / IMX636 Event Camera";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RendererPreference {
    Auto,
    Glow,
    Wgpu,
}

impl RendererPreference {
    pub fn from_env() -> Self {
        match std::env::var("AUGUR_RENDERER") {
            Ok(value) => match value.trim().to_ascii_lowercase().as_str() {
                "glow" => Self::Glow,
                "wgpu" => Self::Wgpu,
                "auto" => Self::Auto,
                other => {
                    eprintln!(
                        "Ignoring unsupported AUGUR_RENDERER={other:?}; expected glow, wgpu, or auto."
                    );
                    Self::Auto
                }
            },
            Err(_) => Self::Auto,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Glow => "glow",
            Self::Wgpu => "wgpu",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveRendererInfo {
    pub requested: RendererPreference,
    pub active_renderer: String,
    pub backend: String,
    pub adapter: String,
}

impl ActiveRendererInfo {
    pub fn from_creation_context(cc: &eframe::CreationContext<'_>) -> Self {
        let requested = RendererPreference::from_env();
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            let adapter_info = render_state.adapter.get_info();
            return Self {
                requested,
                active_renderer: "wgpu".into(),
                backend: format!("{:?}", adapter_info.backend),
                adapter: format!("{} ({:?})", adapter_info.name, adapter_info.device_type),
            };
        }

        Self {
            requested,
            active_renderer: "glow".into(),
            backend: "OpenGL".into(),
            adapter: "glow compatibility backend".into(),
        }
    }
}

pub fn run_camera_app() -> eframe::Result<()> {
    match RendererPreference::from_env() {
        RendererPreference::Glow => run_with_renderer(Renderer::Glow),
        RendererPreference::Wgpu => run_with_renderer(Renderer::Wgpu),
        RendererPreference::Auto => run_with_renderer(Renderer::Wgpu).or_else(|err| {
            eprintln!("WGPU startup failed, retrying with glow compatibility backend: {err}");
            run_with_renderer(Renderer::Glow)
        }),
    }
}

fn run_with_renderer(renderer: Renderer) -> eframe::Result<()> {
    eframe::run_native(
        APP_TITLE,
        build_native_options(renderer),
        Box::new(|cc| Ok(Box::new(CameraApp::new(cc)))),
    )
}

fn load_icon() -> egui::IconData {
    let icon_bytes = include_bytes!("../../assets/logo.png");
    let img = image::load_from_memory(icon_bytes)
        .expect("failed to decode app icon")
        .into_rgba8();
    let (w, h) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width: w,
        height: h,
    }
}

fn build_native_options(renderer: Renderer) -> NativeOptions {
    let mut options = NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 860.0])
            .with_resizable(true)
            .with_icon(load_icon()),
        renderer,
        ..Default::default()
    };

    // egui-wgpu 0.35 moved the instance/adapter knobs into `WgpuSetup` and the
    // presentation knobs into `SurfaceConfig`; the requested behaviour is unchanged.
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    wgpu_options.surface.desired_maximum_frame_latency = Some(1);
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.instance_descriptor.backends =
            egui_wgpu::wgpu::Backends::PRIMARY | egui_wgpu::wgpu::Backends::GL;
        setup.power_preference = egui_wgpu::wgpu::PowerPreference::HighPerformance;
    }
    options.wgpu_options = wgpu_options;

    options
}
