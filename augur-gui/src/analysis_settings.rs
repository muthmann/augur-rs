use augur_core::analysis::hotpixel::HotpixelConfig;

pub fn draw_analysis_settings(ui: &mut egui::Ui, cfg: &mut HotpixelConfig) -> bool {
    let mut changed = false;

    ui.collapsing("Hotpixel Detection", |ui| {
        ui.weak("GUI-only analysis. Identifies pixels that fire at abnormally high rates regardless of scene activity. Detected hot pixels can be copied into the DEM mask. Changes apply immediately and do not touch hardware.");
        changed |= ui.checkbox(&mut cfg.enabled, "Enabled").changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.history_depth, 4..=64).text("Smoothing depth"))
            .on_hover_text("Number of frames used for the exponential moving average. Higher = more stable detection but slower to react to changes.")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.threshold_factor, 2.0..=50.0).text("Threshold factor"))
            .on_hover_text("A pixel is flagged as hot if its event count exceeds this multiple of the global mean. Lower = more aggressive detection (more pixels flagged).")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.min_absolute_count, 1..=100).text("Min absolute count"))
            .on_hover_text("Minimum event count per frame for a pixel to be considered hot. Prevents false positives when overall activity is low.")
            .changed();
    });

    changed
}
