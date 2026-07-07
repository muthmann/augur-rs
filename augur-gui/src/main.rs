mod analysis_runs;
mod app;
mod colormap;
mod export;
mod export_dialog;
mod external_tools;
mod host_views;
mod hotpixel;
mod inspection_3d;
mod investigation;
mod plugin_loader;
mod plugin_settings_ui;
mod point_cloud;
mod preview;
mod preview_perf;
mod preview_renderer;
mod python_ingress;
mod render_backend;
mod settings;
mod theme;
mod toast;
mod viewer_tools;
mod viewer_widget;

fn main() -> eframe::Result<()> {
    render_backend::run_camera_app()
}
