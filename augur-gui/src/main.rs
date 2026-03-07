mod app;
mod plugin;
mod plugin_loader;
mod plugin_settings_ui;
mod plugins;
mod preview;
mod settings;

use app::CameraApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1400.0, 860.0]),
        ..Default::default()
    };

    eframe::run_native(
        "AugurRS — EVK4 / IMX636 Event Camera",
        options,
        Box::new(|cc| Box::new(CameraApp::new(cc))),
    )
}
