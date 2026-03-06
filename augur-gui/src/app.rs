use std::{
    fs,
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, Overlay},
    camera::{DeviceInfo, EventCamera},
    config::CameraConfig,
    pipeline::{
        spawn_pipeline, Evt3CorePreviewDecoder, PipelineController, PipelineOptions, PreviewFrame,
    },
};
use augur_prophesee::evk4::Evk4Camera;

use crate::{
    plugin::{AnalysisPlugin, PluginContext, PluginInput},
    plugins::create_all_plugins,
    preview::frame_to_color_image,
    settings::draw_settings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Idle,
    Previewing,
    Recording,
}

pub struct CameraApp {
    config: CameraConfig,
    output_path: String,
    mode: AppMode,
    controller: Option<PipelineController>,
    texture: Option<egui::TextureHandle>,
    latest_frame: Option<PreviewFrame>,
    plugins: Vec<Box<dyn AnalysisPlugin>>,
    plugin_context: PluginContext,
    analysis_output: AnalysisOutput,
    analysis_notice: Option<String>,
    acq_time_ms: u64,
    acq_dirty: bool,
    config_dirty: bool,
    lock_settings_while_recording: bool,
    mask_x: u16,
    mask_y: u16,
    mask_file: String,
    last_error: Option<String>,
    last_stats_tick: Instant,
    camera_info: Option<DeviceInfo>,
    camera_status: String,
}

impl CameraApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            config: CameraConfig::default(),
            output_path: "./output.raw".into(),
            mode: AppMode::Idle,
            controller: None,
            texture: None,
            latest_frame: None,
            plugins: create_all_plugins(),
            plugin_context: PluginContext::default(),
            analysis_output: AnalysisOutput::default(),
            analysis_notice: None,
            acq_time_ms: 50,
            acq_dirty: false,
            config_dirty: false,
            lock_settings_while_recording: true,
            mask_x: 0,
            mask_y: 0,
            mask_file: String::new(),
            last_error: None,
            last_stats_tick: Instant::now(),
            camera_info: None,
            camera_status: "Camera not probed yet.".into(),
        }
    }

    fn reset_analysis(&mut self) {
        for plugin in &mut self.plugins {
            plugin.reset();
        }
        self.plugin_context.clear();
        self.analysis_output = AnalysisOutput::default();
        self.analysis_notice = None;
    }

    fn plugins_need_raw_events(&self) -> bool {
        self.plugins
            .iter()
            .any(|plugin| plugin.enabled() && plugin.input_kind() == PluginInput::RawEvents)
    }

    fn sync_pipeline_requirements(&self, controller: &PipelineController) {
        controller
            .raw_events_needed
            .store(self.plugins_need_raw_events(), Ordering::Relaxed);
    }

    fn sync_active_pipeline_requirements(&self) {
        if let Some(controller) = &self.controller {
            self.sync_pipeline_requirements(controller);
        }
    }

    fn validated_output_path(&self) -> Result<PathBuf, String> {
        let p = self.output_path.trim();
        if p.is_empty() {
            return Err("output path must not be empty".into());
        }
        Ok(PathBuf::from(p))
    }

    fn probe_camera(&mut self) {
        match Evk4Camera::open_imx636() {
            Ok(camera) => {
                let info = camera.device_info();
                self.camera_status = format!(
                    "Ready: {} {}",
                    info.model,
                    info.serial
                        .as_deref()
                        .map(|s| format!("(serial {s})"))
                        .unwrap_or_default()
                )
                .trim()
                .to_string();
                self.camera_info = Some(info);
                self.last_error = None;
            }
            Err(e) => {
                self.camera_status = format!("Probe failed: {e}");
                self.camera_info = None;
                self.last_error = Some(format!("camera probe failed: {e}"));
            }
        }
    }

    fn start_preview(&mut self) {
        if self.mode != AppMode::Idle {
            return;
        }
        match self.start_pipeline_inner(true) {
            Ok(controller) => {
                self.sync_pipeline_requirements(&controller);
                self.controller = Some(controller);
                self.mode = AppMode::Previewing;
                self.reset_analysis();
                self.config_dirty = false;
                self.acq_dirty = false;
                self.last_error = None;
                self.camera_status = "Previewing.".into();
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    fn start_recording(&mut self) {
        if self.mode == AppMode::Recording {
            return;
        }
        if self.mode == AppMode::Previewing {
            self.stop_pipeline();
        }
        match self.start_pipeline_inner(false) {
            Ok(controller) => {
                self.sync_pipeline_requirements(&controller);
                self.controller = Some(controller);
                self.mode = AppMode::Recording;
                self.reset_analysis();
                self.config_dirty = false;
                self.acq_dirty = false;
                self.last_error = None;
                self.camera_status = "Recording in progress.".into();
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    fn start_pipeline_inner(&mut self, preview_only: bool) -> Result<PipelineController, String> {
        let options = if preview_only {
            PipelineOptions::preview_only(1280, 720)
        } else {
            let output_path = self.validated_output_path()?;
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("failed creating output directory {}: {e}", parent.display())
                })?;
            }
            let mut opts = PipelineOptions::new(output_path);
            opts.sensor_width = 1280;
            opts.sensor_height = 720;
            opts
        };

        let camera = Evk4Camera::open_imx636().map_err(|e| format!("open camera failed: {e}"))?;
        self.camera_info = Some(camera.device_info());

        let controller = spawn_pipeline(
            camera,
            Evt3CorePreviewDecoder::default(),
            self.config.clone(),
            options,
        )
        .map_err(|e| format!("pipeline start failed: {e}"))?;
        controller
            .acq_time_us
            .store(self.acq_time_ms * 1_000, Ordering::Relaxed);
        Ok(controller)
    }

    fn stop_pipeline(&mut self) {
        if let Some(controller) = self.controller.take() {
            if let Err(e) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {e}"));
            }
        }
        self.texture = None;
        self.latest_frame = None;
        self.reset_analysis();
        self.mode = AppMode::Idle;
        self.camera_status =
            "Camera idle. Current local settings will be used for the next recording.".into();
    }

    fn poll_pipeline_error(&mut self) {
        let maybe_error = self
            .controller
            .as_ref()
            .and_then(|ctrl| ctrl.try_recv_error());
        if let Some(err) = maybe_error {
            self.last_error = Some(err);
            self.stop_pipeline();
        }
    }

    fn apply_runtime_changes(&mut self) {
        let Some(ctrl) = &self.controller else {
            return;
        };

        if self.config_dirty {
            if let Err(e) = self.config.validate(1280, 720) {
                self.last_error = Some(format!("settings invalid: {e}"));
                return;
            }
            if let Err(e) = ctrl.settings_tx.try_send(self.config.clone()) {
                self.last_error = Some(format!("failed sending runtime settings: {e}"));
                return;
            }
            self.config_dirty = false;
        }

        if self.acq_dirty {
            ctrl.acq_time_us
                .store(self.acq_time_ms.saturating_mul(1_000), Ordering::Relaxed);
            self.acq_dirty = false;
        }

        self.last_error = None;
    }

    fn run_analysis_plugins(&mut self, frame: &PreviewFrame) {
        self.analysis_output = AnalysisOutput::default();
        self.plugin_context.clear();
        if self.plugins_need_raw_events() {
            self.plugin_context.raw_events = frame.events.clone();
        }

        for phase in [
            PluginInput::FrameOnly,
            PluginInput::RawEvents,
            PluginInput::DerivedData,
        ] {
            for plugin in &mut self.plugins {
                if plugin.enabled() && plugin.input_kind() == phase {
                    plugin.process_frame_with_context(
                        frame,
                        &mut self.analysis_output,
                        &mut self.plugin_context,
                    );
                }
            }
        }
    }

    fn update_preview_texture(&mut self, ctx: &egui::Context) {
        let Some(ctrl) = &self.controller else {
            return;
        };

        let mut newest_frame = None;
        while let Ok(frame) = ctrl.frame_rx.try_recv() {
            newest_frame = Some(frame);
        }

        let Some(frame) = newest_frame else {
            return;
        };

        self.run_analysis_plugins(&frame);

        let image = frame_to_color_image(&frame, &self.analysis_output.overlays);
        self.latest_frame = Some(frame);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        }
    }

    fn settings_are_locked(&self) -> bool {
        self.mode == AppMode::Recording && self.lock_settings_while_recording
    }

    fn latest_detected_hotpixels(&self) -> Vec<(u16, u16)> {
        let mut pixels = Vec::new();
        for overlay in &self.analysis_output.overlays {
            if let Overlay::HighlightPixels {
                pixels: highlighted,
                ..
            } = overlay
            {
                for pixel in highlighted {
                    let coords = (pixel.x, pixel.y);
                    if !pixels.contains(&coords) {
                        pixels.push(coords);
                    }
                }
            }
        }
        pixels
    }

    fn copy_detected_hotpixels_to_mask(&mut self) {
        const IMX636_DEM_SLOTS: usize = 64;

        let detected = self.latest_detected_hotpixels();
        if detected.is_empty() {
            self.analysis_notice = Some("No detected hotpixels are available to copy.".into());
            return;
        }

        let mut added = 0;
        let mut duplicates = 0;
        let mut capacity_skipped = 0;

        for pixel in detected {
            if self.config.pixel_mask.masked_pixels.contains(&pixel) {
                duplicates += 1;
                continue;
            }
            if self.config.pixel_mask.masked_pixels.len() >= IMX636_DEM_SLOTS {
                capacity_skipped += 1;
                continue;
            }
            self.config.pixel_mask.masked_pixels.push(pixel);
            added += 1;
        }

        if added > 0 {
            self.config_dirty = self.controller.is_some();
        }

        self.analysis_notice = Some(format!(
            "Mask copy: added {added}, duplicates {duplicates}, skipped by DEM limit {capacity_skipped}."
        ));
    }
}

impl eframe::App for CameraApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_pipeline_error();
        self.update_preview_texture(ctx);

        let mode = self.mode;
        let mut plugin_toggle_changed = false;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(mode == AppMode::Idle, egui::Button::new("Probe Camera"))
                    .clicked()
                {
                    self.probe_camera();
                }

                if ui
                    .add_enabled(
                        mode == AppMode::Idle && self.camera_info.is_some(),
                        egui::Button::new("Preview"),
                    )
                    .clicked()
                {
                    self.start_preview();
                }

                if ui
                    .add_enabled(
                        mode == AppMode::Idle || mode == AppMode::Previewing,
                        egui::Button::new("Record"),
                    )
                    .clicked()
                {
                    self.start_recording();
                }

                if ui
                    .add_enabled(
                        mode == AppMode::Previewing || mode == AppMode::Recording,
                        egui::Button::new("Stop"),
                    )
                    .clicked()
                {
                    self.stop_pipeline();
                }

                if ui
                    .add_enabled(
                        (mode == AppMode::Previewing || mode == AppMode::Recording)
                            && !self.settings_are_locked(),
                        egui::Button::new("Apply Settings"),
                    )
                    .clicked()
                {
                    self.apply_runtime_changes();
                }

                ui.separator();
                ui.menu_button("Analysis", |ui| {
                    for plugin in &mut self.plugins {
                        let mut enabled = plugin.enabled();
                        if ui.checkbox(&mut enabled, plugin.name()).changed() {
                            plugin.set_enabled(enabled);
                            plugin_toggle_changed = true;
                        }
                    }
                });

                ui.separator();

                let output_enabled = mode != AppMode::Recording;
                ui.label("Output");
                ui.add_enabled(
                    output_enabled,
                    egui::TextEdit::singleline(&mut self.output_path).desired_width(200.0),
                );
                if ui
                    .add_enabled(output_enabled, egui::Button::new("Browse…"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("output.raw")
                        .add_filter("RAW", &["raw"])
                        .save_file()
                    {
                        self.output_path = path.display().to_string();
                    }
                }

                ui.separator();

                if ui.button("Save Config").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_file_name("augur.toml")
                        .add_filter("TOML", &["toml"])
                        .save_file()
                    {
                        if let Err(e) = self.config.save_to_path(&path) {
                            self.last_error = Some(format!("save config failed: {e}"));
                        }
                    }
                }

                if ui
                    .add_enabled(mode != AppMode::Recording, egui::Button::new("Load Config"))
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("TOML", &["toml"])
                        .pick_file()
                    {
                        match CameraConfig::load_from_path(&path) {
                            Ok(cfg) => {
                                self.mask_file = cfg
                                    .pixel_mask
                                    .mask_file
                                    .as_ref()
                                    .map(|p| p.display().to_string())
                                    .unwrap_or_default();
                                self.config = cfg;
                                self.config_dirty = self.controller.is_some();
                                self.last_error = None;
                            }
                            Err(e) => {
                                self.last_error = Some(format!("load config failed: {e}"));
                            }
                        }
                    }
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Camera: {}", self.camera_status));
                ui.separator();
                ui.label(format!("Output: {}", self.output_path.trim()));
                if mode == AppMode::Recording {
                    ui.separator();
                    ui.label("Output path changes apply only to the next recording.");
                }
            });
        });

        if plugin_toggle_changed {
            self.analysis_output = AnalysisOutput::default();
            self.analysis_notice = None;
            self.sync_active_pipeline_requirements();
        }

        egui::SidePanel::left("settings")
            .min_width(340.0)
            .show(ctx, |ui| {
                let locked = self.settings_are_locked();

                ui.heading("Settings");
                ui.separator();
                ui.checkbox(
                    &mut self.lock_settings_while_recording,
                    "Lock settings while recording",
                );

                match mode {
                    AppMode::Recording if locked => {
                        ui.label("Recording: settings are locked.");
                    }
                    AppMode::Recording => {
                        ui.label("Recording: edits stay local until you click Apply Settings.");
                    }
                    AppMode::Previewing => {
                        ui.label("Previewing: edits stay local until you click Apply Settings.");
                    }
                    AppMode::Idle => {
                        ui.label("Idle: edits change the local config for the next recording.");
                    }
                }

                ui.separator();
                ui.add_enabled_ui(!locked, |ui| {
                    let changed = draw_settings(
                        ui,
                        &mut self.config,
                        &mut self.mask_x,
                        &mut self.mask_y,
                        &mut self.mask_file,
                    );
                    if changed {
                        self.config_dirty = true;
                    }
                });
            });

        let enabled_plugin_names: Vec<String> = self
            .plugins
            .iter()
            .filter(|plugin| plugin.enabled())
            .map(|plugin| plugin.name().to_owned())
            .collect();
        let mut plugin_config_changed = false;
        if !enabled_plugin_names.is_empty() {
            egui::SidePanel::right("analysis")
                .min_width(360.0)
                .show(ctx, |ui| {
                    ui.heading("Analysis Tools");
                    ui.separator();

                    let (plugins, config) = (&mut self.plugins, &mut self.config);
                    for plugin in plugins.iter_mut() {
                        if !plugin.enabled() {
                            continue;
                        }

                        let missing_dependencies: Vec<&str> = plugin
                            .dependencies()
                            .iter()
                            .copied()
                            .filter(|dependency| {
                                !enabled_plugin_names.iter().any(|name| name == dependency)
                            })
                            .collect();
                        if !missing_dependencies.is_empty() {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                format!("Missing dependency: {}", missing_dependencies.join(", ")),
                            );
                        }

                        if plugin.ui_settings(ui, config) {
                            plugin_config_changed = true;
                        }

                        ui.separator();
                    }
                });
        }

        if plugin_config_changed {
            self.config_dirty = true;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Live Preview");
            ui.separator();

            if let Some(info) = &self.camera_info {
                ui.label(format!(
                    "{} | serial: {} | firmware: {}",
                    info.compatible.as_deref().unwrap_or(&info.model),
                    info.serial.as_deref().unwrap_or("-"),
                    info.firmware.as_deref().unwrap_or("-"),
                ));
            }

            if let Some(texture) = &self.texture {
                let available = ui.available_size();
                let size = texture.size_vec2();
                let scale = (available.x / size.x).min(available.y / size.y).max(0.1);
                ui.add(egui::Image::new(texture).fit_to_exact_size(size * scale));
            } else {
                ui.label("No preview yet. Probe the camera, then click Preview or Record.");
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Acq time [ms]");
                let changed = ui
                    .add_enabled(
                        !self.settings_are_locked(),
                        egui::Slider::new(&mut self.acq_time_ms, 1..=1000),
                    )
                    .changed();
                if changed {
                    self.acq_dirty = true;
                }
                if mode != AppMode::Idle {
                    if self.settings_are_locked() {
                        ui.label("Preview changes are locked during recording.");
                    } else {
                        ui.label("Click Apply Settings to push preview timing.");
                    }
                }
            });

            if let Some(ctrl) = &self.controller {
                if self.last_stats_tick.elapsed() >= Duration::from_millis(250) {
                    self.last_stats_tick = Instant::now();
                }
                let s = ctrl.stats_snapshot();
                ui.label(format!(
                    "{:.2} Mev/s current | {:.2} MB/s current | {:02}:{:02}:{:02} elapsed",
                    s.mev_per_s,
                    s.mb_per_s,
                    (s.elapsed_s as u64) / 3600,
                    (s.elapsed_s as u64 % 3600) / 60,
                    s.elapsed_s as u64 % 60
                ));
            }

            if mode != AppMode::Idle {
                if self.config_dirty || self.acq_dirty {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "There are unapplied runtime changes.",
                    );
                } else {
                    ui.label("Runtime settings on the camera are up to date.");
                }
            }

            if !self.analysis_output.warnings.is_empty() {
                ui.separator();
                for warning in &self.analysis_output.warnings {
                    ui.colored_label(
                        analysis_warning_color(warning.severity),
                        format!("{}: {}", warning.source, warning.message),
                    );
                }

                let detected_pixels = self.latest_detected_hotpixels();
                if !detected_pixels.is_empty() {
                    let can_copy = !self.settings_are_locked();
                    if ui
                        .add_enabled(can_copy, egui::Button::new("Mask detected hotpixels"))
                        .clicked()
                    {
                        self.copy_detected_hotpixels_to_mask();
                    }
                    if !can_copy {
                        ui.label("Unlock runtime settings to copy detections into the DEM mask.");
                    }
                }
            }

            if let Some(notice) = &self.analysis_notice {
                ui.label(notice);
            }

            if let Some(err) = &self.last_error {
                ui.separator();
                ui.colored_label(egui::Color32::RED, err);
            }
        });

        if self.mode != AppMode::Idle {
            ctx.request_repaint();
        }
    }
}

fn analysis_warning_color(severity: AnalysisSeverity) -> egui::Color32 {
    match severity {
        AnalysisSeverity::Info => egui::Color32::from_rgb(96, 160, 255),
        AnalysisSeverity::Warning => egui::Color32::YELLOW,
        AnalysisSeverity::Error => egui::Color32::RED,
    }
}

impl Drop for CameraApp {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}
