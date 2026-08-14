mod analysis_runs;
mod app;
mod colormap;
mod diagnostics;
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
mod profile_store;
mod python_ingress;
mod render_backend;
mod settings;
mod theme;
mod toast;
mod updates;
mod viewer_tools;
mod viewer_widget;

fn main() -> eframe::Result<()> {
    // First, before anything can fail: a crash during startup is a crash worth
    // recording, and until this runs the program cannot report anything at all.
    if let Some(log) = diagnostics::install() {
        eprintln!("augur: session log → {}", log.display());
    }
    let result = render_backend::run_camera_app();
    // Reached on both Ok and Err — an `eframe` error is an orderly exit, not the
    // silent death the breadcrumb is looking for.
    diagnostics::mark_clean_exit();
    result
}
