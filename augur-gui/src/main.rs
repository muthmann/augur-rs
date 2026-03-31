mod app;
mod colormap;
mod external_tools;
mod host_views;
mod plugin;
mod plugin_loader;
mod plugin_settings_ui;
mod plugins;
mod point_cloud;
mod preview;
mod settings;
mod viewer_tools;
mod viewer_widget;

use app::CameraApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 860.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "AugurRS — EVK4 / IMX636 Event Camera",
        options,
        Box::new(|cc| Box::new(CameraApp::new(cc))),
    )
}
