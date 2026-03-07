use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, Overlay},
    camera::{DeviceInfo, EventCamera},
    config::CameraConfig,
    pipeline::{
        spawn_pipeline, Evt3CorePreviewDecoder, PipelineController, PipelineOptions, PreviewFrame,
    },
    replay::{align_relative_evt3_word_offset, RawFileCamera, ReplayControls, ReplayFileInfo},
};
use augur_plugin_api::PluginInput;
use augur_prophesee::evk4::Evk4Camera;

use crate::{
    plugin::AnalysisPlugin, plugin_loader::PluginManager,
    plugin_settings_ui::render_plugin_settings, plugins::create_all_plugins,
    preview::frame_to_color_image, settings::draw_settings,
};

const REPLAY_SPEED_OPTIONS: [(f32, &str); 6] = [
    (0.25, "0.25x"),
    (0.5, "0.5x"),
    (1.0, "1x"),
    (2.0, "2x"),
    (4.0, "4x"),
    (f32::INFINITY, "Max"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    Idle,
    Previewing,
    Recording,
    Replaying,
}

#[derive(Debug, Clone)]
struct SavedLiveState {
    config: CameraConfig,
    mask_file: String,
    camera_info: Option<DeviceInfo>,
}

pub struct CameraApp {
    config: CameraConfig,
    output_path: String,
    replay_path: Option<String>,
    mode: AppMode,
    controller: Option<PipelineController>,
    texture: Option<egui::TextureHandle>,
    latest_frame: Option<PreviewFrame>,
    replay_controls: Option<ReplayControls>,
    replay_file_info: Option<ReplayFileInfo>,
    replay_paused: bool,
    replay_finished: bool,
    replay_pause_after_seek_frame: bool,
    replay_speed: f32,
    replay_notice: Option<String>,
    replay_seek_drag_value: Option<f32>,
    saved_live_state: Option<SavedLiveState>,
    builtin_plugins: Vec<Box<dyn AnalysisPlugin>>,
    plugin_manager: PluginManager,
    plugin_context_data: HashMap<String, Vec<u8>>,
    analysis_output: AnalysisOutput,
    analysis_notice: Option<String>,
    acq_time_ms: u64,
    acq_dirty: bool,
    config_dirty: bool,
    contrast_percentile: f32,
    lock_settings_while_recording: bool,
    settings_panel_open: bool,
    analysis_panel_open: bool,
    plugins_window_open: bool,
    mask_x: u16,
    mask_y: u16,
    mask_file: String,
    last_error: Option<String>,
    camera_info: Option<DeviceInfo>,
    camera_status: String,
}

impl CameraApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut plugin_manager = PluginManager::new_default();
        let plugin_scan_error = plugin_manager.scan_and_load().err();

        Self {
            config: CameraConfig::default(),
            output_path: "./output.raw".into(),
            replay_path: None,
            mode: AppMode::Idle,
            controller: None,
            texture: None,
            latest_frame: None,
            replay_controls: None,
            replay_file_info: None,
            replay_paused: false,
            replay_finished: false,
            replay_pause_after_seek_frame: false,
            replay_speed: 1.0,
            replay_notice: None,
            replay_seek_drag_value: None,
            saved_live_state: None,
            builtin_plugins: create_all_plugins(),
            plugin_manager,
            plugin_context_data: HashMap::new(),
            analysis_output: AnalysisOutput::default(),
            analysis_notice: None,
            acq_time_ms: 50,
            acq_dirty: false,
            config_dirty: false,
            contrast_percentile: 99.5,
            lock_settings_while_recording: true,
            settings_panel_open: true,
            analysis_panel_open: true,
            plugins_window_open: false,
            mask_x: 0,
            mask_y: 0,
            mask_file: String::new(),
            last_error: plugin_scan_error,
            camera_info: None,
            camera_status: "Camera not probed yet.".into(),
        }
    }

    fn reset_analysis(&mut self) {
        for plugin in &mut self.builtin_plugins {
            plugin.reset();
        }
        for record in self.plugin_manager.records_mut() {
            if let Some(plugin) = record.plugin_mut() {
                plugin.reset();
            }
        }
        self.plugin_context_data.clear();
        self.analysis_output = AnalysisOutput::default();
        self.analysis_notice = None;
    }

    fn plugins_need_raw_events(&self) -> bool {
        self.builtin_plugins
            .iter()
            .any(|plugin| plugin.enabled() && plugin.input_kind() == PluginInput::RawEvents)
            || self.plugin_manager.records().iter().any(|record| {
                record.plugin().is_some_and(|plugin| {
                    plugin.enabled() && plugin.input_kind() == PluginInput::RawEvents
                })
            })
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

    fn enabled_plugin_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .builtin_plugins
            .iter()
            .filter(|plugin| plugin.enabled())
            .map(|plugin| plugin.name().to_owned())
            .collect();
        names.extend(
            self.plugin_manager
                .records()
                .iter()
                .filter_map(|record| record.plugin())
                .filter(|plugin| plugin.enabled())
                .map(|plugin| plugin.name().to_owned()),
        );
        names
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

    fn open_replay_file(&mut self) {
        if self.mode != AppMode::Idle {
            return;
        }

        let Some(path) = rfd::FileDialog::new()
            .add_filter("RAW", &["raw"])
            .pick_file()
        else {
            return;
        };

        let saved_live_state = SavedLiveState {
            config: self.config.clone(),
            mask_file: self.mask_file.clone(),
            camera_info: self.camera_info.clone(),
        };

        let (camera, controls, info) = match RawFileCamera::open(&path) {
            Ok(result) => result,
            Err(err) => {
                self.last_error = Some(format!("open replay file failed: {err}"));
                return;
            }
        };
        let (display_config, display_mask_file, replay_notice) =
            self.load_replay_display_settings(&path, info.width, info.height);
        let replay_info = camera.device_info();
        let controller = match spawn_pipeline(
            camera,
            Evt3CorePreviewDecoder::default(),
            replay_pipeline_config(&info),
            PipelineOptions::preview_only(info.width, info.height),
        ) {
            Ok(controller) => controller,
            Err(err) => {
                self.last_error = Some(format!("pipeline start failed: {err}"));
                return;
            }
        };

        self.set_replay_paused_internal(&controls, false);
        self.set_replay_speed_internal(&controls, 1.0);
        self.sync_pipeline_requirements(&controller);
        self.controller = Some(controller);
        self.mode = AppMode::Replaying;
        self.camera_info = Some(replay_info);
        self.config = display_config;
        self.mask_file = display_mask_file;
        self.replay_notice = replay_notice;
        self.replay_path = Some(path.display().to_string());
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_paused = false;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = false;
        self.replay_speed = 1.0;
        self.replay_seek_drag_value = None;
        self.saved_live_state = Some(saved_live_state);
        self.reset_analysis();
        self.config_dirty = false;
        self.acq_dirty = false;
        self.last_error = None;
        self.camera_status = format!(
            "Replaying {}.",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
    }

    fn load_replay_display_settings(
        &self,
        raw_path: &Path,
        width: u16,
        height: u16,
    ) -> (CameraConfig, String, Option<String>) {
        let mut default_config = CameraConfig::default();
        default_config.roi.width = width;
        default_config.roi.height = height;

        let Some(config_path) = replay_config_path(raw_path) else {
            return (
                default_config,
                String::new(),
                Some("Replay sidecar path could not be derived; showing default settings.".into()),
            );
        };

        if !config_path.is_file() {
            return (
                default_config,
                String::new(),
                Some("No companion .toml sidecar found; showing default settings.".into()),
            );
        }

        match CameraConfig::load_from_path(&config_path) {
            Ok(config) => {
                let mask_file = config
                    .pixel_mask
                    .mask_file
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                (config, mask_file, None)
            }
            Err(err) => (
                default_config,
                String::new(),
                Some(format!(
                    "Replay sidecar {} could not be loaded: {err}",
                    config_path.display()
                )),
            ),
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

    fn set_replay_paused_internal(&self, controls: &ReplayControls, paused: bool) {
        controls.paused.store(paused, Ordering::Relaxed);
    }

    fn set_replay_speed_internal(&self, controls: &ReplayControls, speed: f32) {
        controls
            .speed_bits
            .store(speed.to_bits(), Ordering::Relaxed);
    }

    fn set_replay_paused(&mut self, paused: bool) {
        let Some(controls) = &self.replay_controls else {
            return;
        };
        self.set_replay_paused_internal(controls, paused);
        self.replay_paused = paused;
        if !paused {
            self.replay_pause_after_seek_frame = false;
        }
    }

    fn set_replay_speed(&mut self, speed: f32) {
        let Some(controls) = &self.replay_controls else {
            return;
        };
        self.set_replay_speed_internal(controls, speed);
        self.replay_speed = speed;
    }

    fn seek_replay(&mut self, fraction: f32) {
        if self.mode != AppMode::Replaying {
            return;
        }

        let Some(path) = self.replay_path.as_ref().map(PathBuf::from) else {
            self.last_error = Some("replay path is missing".into());
            return;
        };
        let Some(info) = self.replay_file_info.clone() else {
            self.last_error = Some("replay file metadata is missing".into());
            return;
        };
        let desired_paused = self.replay_paused || self.replay_finished;

        if let Some(controller) = self.controller.take() {
            if let Err(err) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {err}"));
            }
        }

        let data_len = info.data_len();
        let fraction = fraction.clamp(0.0, 1.0);
        let target_rel = ((data_len as f64 * fraction as f64) as u64).min(data_len);
        let target_byte = info.data_offset + align_relative_evt3_word_offset(target_rel);

        let (camera, controls) = match RawFileCamera::open_at(&path, &info, target_byte) {
            Ok(result) => result,
            Err(err) => {
                self.replay_finished = true;
                self.replay_paused = true;
                self.replay_seek_drag_value = None;
                self.last_error = Some(format!("seek failed: {err}"));
                if let Some(existing_controls) = &self.replay_controls {
                    existing_controls.paused.store(true, Ordering::Relaxed);
                }
                return;
            }
        };

        let controller = match spawn_pipeline(
            camera,
            Evt3CorePreviewDecoder::default(),
            replay_pipeline_config(&info),
            PipelineOptions::preview_only(info.width, info.height),
        ) {
            Ok(controller) => controller,
            Err(err) => {
                self.replay_finished = true;
                self.replay_paused = true;
                self.replay_seek_drag_value = None;
                self.last_error = Some(format!("seek pipeline start failed: {err}"));
                return;
            }
        };

        let pause_after_first_frame = desired_paused;
        self.set_replay_paused_internal(&controls, false);
        self.set_replay_speed_internal(&controls, self.replay_speed);
        self.sync_pipeline_requirements(&controller);
        self.controller = Some(controller);
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_paused = desired_paused;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = pause_after_first_frame;
        self.replay_seek_drag_value = None;
        self.last_error = None;
        self.camera_status = format!(
            "Replaying {}.",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
        self.latest_frame = None;
        self.reset_analysis();
    }

    fn restart_replay(&mut self) {
        self.seek_replay(0.0);
    }

    fn restore_saved_live_state(&mut self) {
        if let Some(saved) = self.saved_live_state.take() {
            self.config = saved.config;
            self.mask_file = saved.mask_file;
            self.camera_info = saved.camera_info;
        }
    }

    fn clear_replay_state(&mut self) {
        self.replay_controls = None;
        self.replay_file_info = None;
        self.replay_paused = false;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = false;
        self.replay_speed = 1.0;
        self.replay_notice = None;
        self.replay_path = None;
        self.replay_seek_drag_value = None;
        self.restore_saved_live_state();
    }

    fn stop_pipeline(&mut self) {
        let was_replaying = self.mode == AppMode::Replaying;
        if let Some(controller) = self.controller.take() {
            if let Err(e) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {e}"));
            }
        }
        if was_replaying {
            self.clear_replay_state();
        }
        self.texture = None;
        self.latest_frame = None;
        self.reset_analysis();
        self.mode = AppMode::Idle;
        self.camera_status =
            "Camera idle. Current local settings will be used for the next recording.".into();
    }

    fn finish_replay(&mut self) {
        if let Some(controller) = self.controller.take() {
            if let Err(err) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {err}"));
            }
        }

        if let (Some(controls), Some(info)) = (&self.replay_controls, &self.replay_file_info) {
            controls.paused.store(true, Ordering::Relaxed);
            controls
                .bytes_read
                .store(info.data_len(), Ordering::Relaxed);
        }

        self.replay_finished = true;
        self.replay_paused = true;
        self.replay_pause_after_seek_frame = false;
        self.replay_seek_drag_value = None;
        if self.last_error.is_none() {
            self.camera_status = "Replay finished.".into();
        }
    }

    fn poll_pipeline_state(&mut self) {
        let maybe_error = self
            .controller
            .as_ref()
            .and_then(|ctrl| ctrl.try_recv_error());
        if let Some(err) = maybe_error {
            self.last_error = Some(err);
            self.stop_pipeline();
            return;
        }

        let replay_finished = self.mode == AppMode::Replaying
            && self
                .controller
                .as_ref()
                .is_some_and(|ctrl| ctrl.is_stopped() && ctrl.frame_rx.is_empty());
        if replay_finished {
            self.last_error = None;
            self.finish_replay();
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
        self.plugin_context_data.clear();

        for phase in [
            PluginInput::FrameOnly,
            PluginInput::RawEvents,
            PluginInput::DerivedData,
        ] {
            for plugin in &mut self.builtin_plugins {
                if plugin.enabled() && plugin.input_kind() == phase {
                    plugin.process_frame(frame, &mut self.analysis_output);
                }
            }

            for record in self.plugin_manager.records_mut() {
                let Some(plugin) = record.plugin_mut() else {
                    continue;
                };
                if plugin.enabled() && plugin.input_kind() == phase {
                    plugin.process_frame(
                        frame,
                        &mut self.analysis_output,
                        &mut self.plugin_context_data,
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

        if self.mode == AppMode::Replaying && self.replay_pause_after_seek_frame {
            if let Some(controls) = &self.replay_controls {
                self.set_replay_paused_internal(controls, true);
            }
            self.replay_pause_after_seek_frame = false;
            self.replay_paused = true;
        }

        self.run_analysis_plugins(&frame);

        let image = frame_to_color_image(
            &frame,
            &self.analysis_output.overlays,
            self.contrast_percentile,
        );
        self.latest_frame = Some(frame);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        }
    }

    fn settings_are_locked(&self) -> bool {
        self.mode == AppMode::Replaying
            || (self.mode == AppMode::Recording && self.lock_settings_while_recording)
    }

    fn current_replay_fraction(&self) -> f32 {
        if let Some(drag_value) = self.replay_seek_drag_value {
            return drag_value.clamp(0.0, 1.0);
        }

        let Some(controls) = &self.replay_controls else {
            return 0.0;
        };
        let total_bytes = controls.file_size.saturating_sub(controls.data_offset);
        if total_bytes == 0 {
            return 1.0;
        }

        (controls.bytes_read.load(Ordering::Relaxed) as f32 / total_bytes as f32).clamp(0.0, 1.0)
    }

    fn current_replay_bytes_read(&self) -> u64 {
        self.replay_controls
            .as_ref()
            .map(|controls| controls.bytes_read.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn current_replay_time_us(&self) -> u64 {
        let Some(info) = &self.replay_file_info else {
            return 0;
        };
        if self.replay_finished {
            return info.total_duration_us;
        }
        if let Some(frame) = &self.latest_frame {
            return frame
                .window_end_us
                .saturating_sub(info.first_timestamp_us)
                .min(info.total_duration_us);
        }

        ((self.current_replay_fraction() as f64 * info.total_duration_us as f64).round() as u64)
            .min(info.total_duration_us)
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

        if added > 0 && self.mode != AppMode::Replaying {
            self.config_dirty = self.controller.is_some();
        }

        self.analysis_notice = Some(format!(
            "Mask copy: added {added}, duplicates {duplicates}, skipped by DEM limit {capacity_skipped}."
        ));
    }
}

impl eframe::App for CameraApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_preview_texture(ctx);
        self.poll_pipeline_state();

        let mode = self.mode;
        let mut plugin_toggle_changed = false;
        let mut plugin_scan_requested = false;
        let mut open_plugins_dir_requested = false;
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
                    .add_enabled(mode == AppMode::Idle, egui::Button::new("Open .raw"))
                    .clicked()
                {
                    self.open_replay_file();
                }

                if ui
                    .add_enabled(mode != AppMode::Idle, egui::Button::new("Stop"))
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
                if ui
                    .selectable_label(self.settings_panel_open, "Settings Panel")
                    .clicked()
                {
                    self.settings_panel_open = !self.settings_panel_open;
                }
                if ui
                    .selectable_label(self.analysis_panel_open, "Analysis Panel")
                    .clicked()
                {
                    self.analysis_panel_open = !self.analysis_panel_open;
                }
                ui.menu_button("Analysis", |ui| {
                    for plugin in &mut self.builtin_plugins {
                        let mut enabled = plugin.enabled();
                        if ui.checkbox(&mut enabled, plugin.name()).changed() {
                            plugin.set_enabled(enabled);
                            plugin_toggle_changed = true;
                        }
                    }
                    for record in self.plugin_manager.records_mut() {
                        let Some(plugin) = record.plugin_mut() else {
                            continue;
                        };
                        let mut enabled = plugin.enabled();
                        if ui.checkbox(&mut enabled, plugin.name()).changed() {
                            plugin.set_enabled(enabled);
                            plugin_toggle_changed = true;
                        }
                    }
                });
                ui.menu_button("Plugins", |ui| {
                    if ui
                        .button(if self.plugins_window_open {
                            "Hide Plugin Manager"
                        } else {
                            "Show Plugin Manager"
                        })
                        .clicked()
                    {
                        self.plugins_window_open = !self.plugins_window_open;
                        ui.close_menu();
                    }
                    if ui.button("Scan for New Plugins").clicked() {
                        plugin_scan_requested = true;
                        ui.close_menu();
                    }
                    if ui.button("Open Plugins Folder").clicked() {
                        open_plugins_dir_requested = true;
                        ui.close_menu();
                    }
                });

                ui.separator();

                let output_enabled = mode != AppMode::Recording && mode != AppMode::Replaying;
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

                if ui
                    .add_enabled(mode != AppMode::Replaying, egui::Button::new("Save Config"))
                    .clicked()
                {
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
                    .add_enabled(
                        mode != AppMode::Recording && mode != AppMode::Replaying,
                        egui::Button::new("Load Config"),
                    )
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
                if let Some(replay_path) = &self.replay_path {
                    ui.label(format!("Replay: {replay_path}"));
                } else {
                    ui.label(format!("Output: {}", self.output_path.trim()));
                }
                if mode == AppMode::Recording {
                    ui.separator();
                    ui.label("Output path changes apply only to the next recording.");
                }
            });
        });

        if plugin_scan_requested {
            match self.plugin_manager.scan_and_load() {
                Ok(()) => {
                    self.last_error = None;
                    self.reset_analysis();
                    plugin_toggle_changed = true;
                }
                Err(err) => self.last_error = Some(err),
            }
        }

        if open_plugins_dir_requested {
            if let Err(err) = self.plugin_manager.open_plugins_dir() {
                self.last_error = Some(err);
            }
        }

        if plugin_toggle_changed {
            self.analysis_output = AnalysisOutput::default();
            self.analysis_notice = None;
            self.sync_active_pipeline_requirements();
        }

        if self.settings_panel_open {
            egui::SidePanel::left("settings")
                .min_width(340.0)
                .show(ctx, |ui| {
                    let locked = self.settings_are_locked();

                    ui.heading("Settings");
                    ui.separator();
                    if mode != AppMode::Replaying {
                        ui.checkbox(
                            &mut self.lock_settings_while_recording,
                            "Lock settings while recording",
                        );
                    }

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
                        AppMode::Replaying => {
                            ui.label("Replay mode: camera settings are shown as read-only reference data.");
                            if let Some(notice) = &self.replay_notice {
                                ui.colored_label(egui::Color32::YELLOW, notice);
                            }
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
        }

        let enabled_plugin_names = self.enabled_plugin_names();
        let mut builtin_plugin_config_changed = false;
        if self.analysis_panel_open && !enabled_plugin_names.is_empty() {
            egui::SidePanel::right("analysis")
                .min_width(360.0)
                .show(ctx, |ui| {
                    ui.heading("Analysis Tools");
                    ui.separator();

                    let (plugins, config) = (&mut self.builtin_plugins, &mut self.config);
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
                            builtin_plugin_config_changed = true;
                        }

                        ui.separator();
                    }

                    for record in self.plugin_manager.records_mut() {
                        let Some(plugin) = record.plugin_mut() else {
                            continue;
                        };
                        if !plugin.enabled() {
                            continue;
                        }

                        let missing_dependencies: Vec<String> = plugin
                            .dependencies()
                            .iter()
                            .filter(|dependency| {
                                !enabled_plugin_names.iter().any(|name| name == *dependency)
                            })
                            .cloned()
                            .collect();
                        if !missing_dependencies.is_empty() {
                            ui.colored_label(
                                egui::Color32::YELLOW,
                                format!("Missing dependency: {}", missing_dependencies.join(", ")),
                            );
                        }

                        if let Err(err) = render_plugin_settings(ui, plugin) {
                            ui.colored_label(
                                egui::Color32::RED,
                                format!("Dynamic plugin UI failed: {err}"),
                            );
                        }

                        ui.separator();
                    }
                });
        }

        if builtin_plugin_config_changed && mode != AppMode::Replaying {
            self.config_dirty = true;
        }

        let mut reload_requested = None;
        let mut rescan_requested = false;
        let mut open_dir_requested = false;
        egui::Window::new("Plugin Manager")
            .open(&mut self.plugins_window_open)
            .resizable(true)
            .default_width(760.0)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!(
                        "Directory: {}",
                        self.plugin_manager.plugins_dir().display()
                    ));
                    if ui.button("Scan for New Plugins").clicked() {
                        rescan_requested = true;
                    }
                    if ui.button("Open Plugins Folder").clicked() {
                        open_dir_requested = true;
                    }
                });
                ui.separator();

                if self.plugin_manager.records().is_empty() {
                    ui.label("No plugins found. Build a plugin cdylib and copy it together with plugin.toml into the plugins directory.");
                    return;
                }

                egui::Grid::new("plugin_manager_grid")
                    .striped(true)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.strong("Name");
                        ui.strong("Version");
                        ui.strong("Domain");
                        ui.strong("Phase");
                        ui.strong("Status");
                        ui.strong("Enabled");
                        ui.strong("Action");
                        ui.end_row();

                        for (index, record) in self.plugin_manager.records_mut().iter_mut().enumerate()
                        {
                            let response = ui.label(record.name());
                            if !record.description().is_empty() {
                                response.on_hover_text(record.description());
                            }
                            ui.label(record.version());
                            ui.label(record.domain());
                            ui.label(record.phase_label());
                            let status_color = if record.load_error().is_some() {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::from_rgb(88, 196, 92)
                            };
                            ui.colored_label(status_color, record.status_label());

                            if let Some(plugin) = record.plugin_mut() {
                                let mut enabled = plugin.enabled();
                                if ui.checkbox(&mut enabled, "").changed() {
                                    plugin.set_enabled(enabled);
                                    plugin_toggle_changed = true;
                                }
                            } else {
                                ui.label("-");
                            }

                            if ui.button("Reload").clicked() {
                                reload_requested = Some(index);
                            }
                            ui.end_row();

                            if let Some(error) = record.load_error() {
                                ui.colored_label(egui::Color32::YELLOW, error);
                                ui.label("");
                                ui.label("");
                                ui.label("");
                                ui.label("");
                                ui.label("");
                                ui.label("");
                                ui.end_row();
                            }
                        }
                    });
            });

        if let Some(index) = reload_requested {
            match self.plugin_manager.reload_plugin(index) {
                Ok(()) => {
                    self.last_error = None;
                    self.reset_analysis();
                    plugin_toggle_changed = true;
                }
                Err(err) => self.last_error = Some(err),
            }
        }

        if rescan_requested {
            match self.plugin_manager.scan_and_load() {
                Ok(()) => {
                    self.last_error = None;
                    self.reset_analysis();
                    plugin_toggle_changed = true;
                }
                Err(err) => self.last_error = Some(err),
            }
        }

        if open_dir_requested {
            if let Err(err) = self.plugin_manager.open_plugins_dir() {
                self.last_error = Some(err);
            }
        }

        if plugin_toggle_changed {
            self.analysis_output = AnalysisOutput::default();
            self.analysis_notice = None;
            self.sync_active_pipeline_requirements();
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(if mode == AppMode::Replaying {
                "Replay"
            } else {
                "Live Preview"
            });
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
                match mode {
                    AppMode::Replaying => {
                        ui.label("No replay frame yet. Use the timeline or wait for playback to decode the next frame.");
                    }
                    _ => {
                        ui.label("No preview yet. Probe the camera, then click Preview or Record.");
                    }
                }
            }

            if mode == AppMode::Replaying {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    let play_pause = if self.replay_paused { "Play" } else { "Pause" };
                    if ui
                        .add_enabled(!self.replay_finished, egui::Button::new(play_pause))
                        .clicked()
                    {
                        self.set_replay_paused(!self.replay_paused);
                    }

                    if ui.button("Restart").clicked() {
                        self.restart_replay();
                    }

                    ui.label("Speed");
                    egui::ComboBox::from_id_source("replay-speed")
                        .selected_text(replay_speed_label(self.replay_speed))
                        .show_ui(ui, |ui| {
                            for (speed, label) in REPLAY_SPEED_OPTIONS {
                                if ui
                                    .selectable_label(
                                        replay_speed_matches(self.replay_speed, speed),
                                        label,
                                    )
                                    .clicked()
                                {
                                    self.set_replay_speed(speed);
                                }
                            }
                        });

                    if self.replay_finished {
                        ui.colored_label(egui::Color32::LIGHT_GREEN, "Finished");
                    }
                });

                if let Some(info) = self.replay_file_info.clone() {
                    let mut timeline_fraction =
                        self.replay_seek_drag_value.unwrap_or_else(|| self.current_replay_fraction());
                    let slider = egui::Slider::new(&mut timeline_fraction, 0.0..=1.0)
                        .show_value(false)
                        .text("Timeline");
                    let slider_width = ui.available_width().max(420.0);
                    let response = ui.add_sized([slider_width, 0.0], slider);
                    if response.dragged() {
                        self.replay_seek_drag_value = Some(timeline_fraction);
                    }
                    if response.drag_stopped() || (response.changed() && !response.dragged()) {
                        self.replay_seek_drag_value = None;
                        self.seek_replay(timeline_fraction);
                    }

                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{} / {}",
                            format_replay_time(self.current_replay_time_us()),
                            format_replay_time(info.total_duration_us)
                        ));
                        ui.separator();
                        ui.label(format!(
                            "{:.1} / {:.1} MB",
                            self.current_replay_bytes_read() as f64 / (1024.0 * 1024.0),
                            info.data_len() as f64 / (1024.0 * 1024.0)
                        ));
                    });
                }
            } else {
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
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Contrast");
                ui.add(egui::Slider::new(&mut self.contrast_percentile, 90.0..=100.0));
                ui.label(format!("{:.1}th percentile", self.contrast_percentile));
            });

            if let Some(ctrl) = &self.controller {
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

            if mode != AppMode::Idle && mode != AppMode::Replaying {
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

        if self.mode != AppMode::Idle && self.controller.is_some() {
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

fn replay_pipeline_config(info: &ReplayFileInfo) -> CameraConfig {
    let mut config = CameraConfig::default();
    config.roi.width = info.width;
    config.roi.height = info.height;
    config
}

fn replay_config_path(raw_path: &Path) -> Option<PathBuf> {
    let stem = raw_path.file_stem()?.to_string_lossy();
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}.toml")))
}

fn replay_speed_label(speed: f32) -> &'static str {
    REPLAY_SPEED_OPTIONS
        .iter()
        .find_map(|(candidate, label)| replay_speed_matches(speed, *candidate).then_some(*label))
        .unwrap_or("Custom")
}

fn replay_speed_matches(current: f32, candidate: f32) -> bool {
    if current.is_infinite() || candidate.is_infinite() {
        current.is_infinite() && candidate.is_infinite()
    } else {
        (current - candidate).abs() < f32::EPSILON
    }
}

fn format_replay_time(duration_us: u64) -> String {
    let total_tenths = ((duration_us as f64) / 100_000.0).round() as u64;
    let minutes = total_tenths / 600;
    let seconds = (total_tenths % 600) / 10;
    let tenths = total_tenths % 10;
    format!("{minutes:02}:{seconds:02}.{tenths}")
}

impl Drop for CameraApp {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}
