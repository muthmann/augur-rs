use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex},
    time::SystemTime,
};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, Overlay},
    camera::{DeviceInfo, EventCamera},
    config::{CameraConfig, RoiConfig},
    pipeline::{
        spawn_pipeline, CdEvent, Evt3CorePreviewDecoder, PipelineController, PipelineOptions,
        PreviewFrame,
    },
    replay::{align_relative_evt3_word_offset, RawFileCamera, ReplayControls, ReplayFileInfo},
    DecodedEventFileCamera, PackedEventPreviewDecoder, PACKED_EVENT_RECORD_BYTES,
};
use augur_plugin_api::PluginInput;
use augur_prophesee::evk4::Evk4Camera;

use crate::{
    plugin::AnalysisPlugin,
    plugin_loader::PluginManager,
    plugin_settings_ui::render_plugin_settings,
    plugins::create_all_plugins,
    point_cloud::{PointCloudMetrics, PointCloudState},
    preview::frame_to_color_image,
    settings::draw_settings,
};

const REPLAY_SPEED_OPTIONS: [(f32, &str); 6] = [
    (0.25, "0.25x"),
    (0.5, "0.5x"),
    (1.0, "1x"),
    (2.0, "2x"),
    (4.0, "4x"),
    (f32::INFINITY, "Max"),
];
const COLLAPSED_PANEL_WIDTH: f32 = 22.0;
pub(crate) const PANEL_ROUNDING: f32 = 6.0;
const PREVIEW_ZOOM_MIN: f32 = 1.0;
const PREVIEW_ZOOM_MAX: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Preview2d,
    PointCloud3d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewTool {
    None,
    SelectRoi,
}

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

#[derive(Debug, Clone)]
struct PreviewWorkspaceState {
    view_mode: ViewMode,
    tool: PreviewTool,
    popup_open: bool,
    zoom: f32,
    pan: egui::Vec2,
    crop_to_roi: bool,
    hover_sensor: Option<(u16, u16)>,
    selection_anchor: Option<egui::Pos2>,
    pending_roi: Option<egui::Rect>,
    point_cloud: PointCloudState,
}

impl Default for PreviewWorkspaceState {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::Preview2d,
            tool: PreviewTool::None,
            popup_open: false,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            crop_to_roi: false,
            hover_sensor: None,
            selection_anchor: None,
            pending_roi: None,
            point_cloud: PointCloudState::default(),
        }
    }
}

impl PreviewWorkspaceState {
    fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.pending_roi = None;
        self.tool = PreviewTool::None;
    }

    fn clear_session_state(&mut self) {
        self.hover_sensor = None;
        self.selection_anchor = None;
        self.pending_roi = None;
        self.point_cloud.clear();
    }

    fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
    }

    fn set_view_mode(&mut self, view_mode: ViewMode) {
        self.view_mode = view_mode;
        if view_mode != ViewMode::Preview2d {
            self.clear_selection();
        }
    }
}

struct PopupSharedData {
    texture: Option<egui::TextureHandle>,
    mode: AppMode,
    view_mode: ViewMode,
    roi: RoiConfig,
    frame_width: u16,
    frame_height: u16,
    // replay
    replay_paused: bool,
    replay_finished: bool,
    replay_speed: f32,
    replay_fraction: f32,
    replay_duration_us: u64,
    replay_time_us: u64,
    replay_bytes_read: u64,
    replay_data_len: u64,
    // actions from popup -> main
    close_requested: bool,
    toggle_pause: bool,
    restart: bool,
    seek_to: Option<f32>,
    set_speed: Option<f32>,
    // popup-internal drag tracking
    seek_drag: Option<f32>,
}

impl Default for PopupSharedData {
    fn default() -> Self {
        Self {
            texture: None,
            mode: AppMode::Idle,
            view_mode: ViewMode::Preview2d,
            roi: RoiConfig {
                x: 0,
                y: 0,
                width: 1280,
                height: 720,
            },
            frame_width: 0,
            frame_height: 0,
            replay_paused: false,
            replay_finished: false,
            replay_speed: 1.0,
            replay_fraction: 0.0,
            replay_duration_us: 0,
            replay_time_us: 0,
            replay_bytes_read: 0,
            replay_data_len: 0,
            close_requested: false,
            toggle_pause: false,
            restart: false,
            seek_to: None,
            set_speed: None,
            seek_drag: None,
        }
    }
}

fn format_timestamp_now() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs();
    // Manual UTC breakdown (no chrono needed)
    let secs_per_day: u64 = 86400;
    let days = total_secs / secs_per_day;
    let day_secs = total_secs % secs_per_day;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    // Days since epoch -> year/month/day (civil calendar)
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

/// Convert days since Unix epoch to (year, month, day).
/// Algorithm from Howard Hinnant (public domain).
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

fn insert_timestamp_suffix(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("raw");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let ts = format_timestamp_now();
    parent.join(format!("{stem}_{ts}.{ext}"))
}

pub struct CameraApp {
    config: CameraConfig,
    output_path: String,
    always_timestamp: bool,
    replay_path: Option<String>,
    mode: AppMode,
    controller: Option<PipelineController>,
    texture: Option<egui::TextureHandle>,
    latest_frame: Option<PreviewFrame>,
    replay_controls: Option<ReplayControls>,
    replay_file_info: Option<ReplayFileInfo>,
    replay_decoded_events: Option<Arc<Vec<CdEvent>>>,
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
    preview_workspace: PreviewWorkspaceState,
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
    popup_shared: Arc<Mutex<PopupSharedData>>,
}

impl CameraApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let mut plugin_manager = PluginManager::new_default();
        let plugin_scan_error = plugin_manager.scan_and_load().err();

        Self {
            config: CameraConfig::default(),
            output_path: format!("./output_{}.raw", format_timestamp_now()),
            always_timestamp: false,
            replay_path: None,
            mode: AppMode::Idle,
            controller: None,
            texture: None,
            latest_frame: None,
            replay_controls: None,
            replay_file_info: None,
            replay_decoded_events: None,
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
            preview_workspace: PreviewWorkspaceState::default(),
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
            popup_shared: Arc::new(Mutex::new(PopupSharedData::default())),
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

    fn raw_events_required(&self) -> bool {
        self.preview_workspace.view_mode == ViewMode::PointCloud3d || self.plugins_need_raw_events()
    }

    fn sync_pipeline_requirements(&self, controller: &PipelineController) {
        controller
            .raw_events_needed
            .store(self.raw_events_required(), Ordering::Relaxed);
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
        let mut path = PathBuf::from(p);

        if self.always_timestamp {
            path = insert_timestamp_suffix(&path);
        }

        if path.exists() {
            path = insert_timestamp_suffix(&path);
        }

        Ok(path)
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
                self.preview_workspace.clear_session_state();
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
                self.preview_workspace.clear_session_state();
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
            .add_filter("Replay Files", &["raw", "csv", "bin", "npy", "h5", "hdf5"])
            .pick_file()
        else {
            return;
        };

        let saved_live_state = SavedLiveState {
            config: self.config.clone(),
            mask_file: self.mask_file.clone(),
            camera_info: self.camera_info.clone(),
        };

        let open_result = match replay_file_extension(&path).as_deref() {
            Some("raw") => match RawFileCamera::open(&path) {
                Ok((camera, controls, info)) => {
                    let replay_info = camera.device_info();
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::default(),
                        replay_pipeline_config(&info),
                        PipelineOptions::preview_only(info.width, info.height),
                    )
                    .map(|controller| (controller, controls, info, replay_info, None))
                    .map_err(|err| format!("pipeline start failed: {err}"))
                }
                Err(err) => Err(format!("open replay file failed: {err}")),
            },
            Some("csv") | Some("bin") | Some("npy") | Some("h5") | Some("hdf5") => {
                match DecodedEventFileCamera::open(&path) {
                    Ok((camera, controls, info, decoded_events)) => {
                        let replay_info = camera.device_info();
                        spawn_pipeline(
                            camera,
                            PackedEventPreviewDecoder::default(),
                            replay_pipeline_config(&info),
                            PipelineOptions::preview_only(info.width, info.height),
                        )
                        .map(|controller| {
                            (
                                controller,
                                controls,
                                info,
                                replay_info,
                                Some(decoded_events),
                            )
                        })
                        .map_err(|err| format!("pipeline start failed: {err}"))
                    }
                    Err(err) => Err(format!("open replay file failed: {err}")),
                }
            }
            Some(ext) => Err(format!("unsupported replay file extension: .{ext}")),
            None => Err("replay file is missing an extension".into()),
        };
        let (controller, controls, info, replay_info, decoded_events) = match open_result {
            Ok(result) => result,
            Err(err) => {
                self.last_error = Some(err);
                return;
            }
        };
        let (display_config, display_mask_file, replay_notice) =
            self.load_replay_display_settings(&path, info.width, info.height);

        self.set_replay_paused_internal(&controls, false);
        self.set_replay_speed_internal(&controls, 1.0);
        self.sync_pipeline_requirements(&controller);
        self.controller = Some(controller);
        self.mode = AppMode::Replaying;
        self.preview_workspace.clear_session_state();
        self.camera_info = Some(replay_info);
        self.config = display_config;
        self.mask_file = display_mask_file;
        self.replay_notice = replay_notice;
        self.replay_path = Some(path.display().to_string());
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_decoded_events = decoded_events;
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
        let decoded_events = self.replay_decoded_events.clone();
        let desired_paused = self.replay_paused || self.replay_finished;

        if let Some(controller) = self.controller.take() {
            if let Err(err) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {err}"));
            }
        }

        let data_len = info.data_len();
        let fraction = fraction.clamp(0.0, 1.0);
        let target_rel = ((data_len as f64 * fraction as f64) as u64).min(data_len);
        let reopen_result = if let Some(decoded_events) = decoded_events {
            let target_byte = target_rel - (target_rel % PACKED_EVENT_RECORD_BYTES as u64);
            match DecodedEventFileCamera::open_at(decoded_events, &info, target_byte) {
                Ok((camera, controls)) => spawn_pipeline(
                    camera,
                    PackedEventPreviewDecoder::default(),
                    replay_pipeline_config(&info),
                    PipelineOptions::preview_only(info.width, info.height),
                )
                .map(|controller| (controller, controls))
                .map_err(|err| format!("seek pipeline start failed: {err}")),
                Err(err) => Err(format!("seek failed: {err}")),
            }
        } else {
            let target_byte = info.data_offset + align_relative_evt3_word_offset(target_rel);
            match RawFileCamera::open_at(&path, &info, target_byte) {
                Ok((camera, controls)) => spawn_pipeline(
                    camera,
                    Evt3CorePreviewDecoder::default(),
                    replay_pipeline_config(&info),
                    PipelineOptions::preview_only(info.width, info.height),
                )
                .map(|controller| (controller, controls))
                .map_err(|err| format!("seek pipeline start failed: {err}")),
                Err(err) => Err(format!("seek failed: {err}")),
            }
        };
        let (controller, controls) = match reopen_result {
            Ok(result) => result,
            Err(err) => {
                self.replay_finished = true;
                self.replay_paused = true;
                self.replay_seek_drag_value = None;
                self.last_error = Some(err);
                if let Some(existing_controls) = &self.replay_controls {
                    existing_controls.paused.store(true, Ordering::Relaxed);
                }
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
        self.preview_workspace.clear_session_state();
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
        self.replay_decoded_events = None;
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
        self.preview_workspace.clear_session_state();
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
        if let Some(events) = frame.events.as_deref() {
            self.preview_workspace.point_cloud.push_events(events);
        }
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

    fn draw_replay_transport(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.replay_file_info.clone() else {
            return;
        };

        // Precompute time label so we can measure its width for the slider.
        let time_text = format!(
            "{} / {}",
            format_replay_time(self.current_replay_time_us()),
            format_replay_time(info.total_duration_us)
        );

        // Collect deferred actions to avoid borrow issues inside closures.
        let mut new_speed: Option<f32> = None;
        let mut stop_requested = false;

        let current_speed = self.replay_speed;

        ui.horizontal(|ui| {
            // Play/Pause
            let play_pause = if self.replay_paused {
                "\u{25B6}"
            } else {
                "\u{23F8}"
            };
            if ui
                .add_enabled(!self.replay_finished, egui::Button::new(play_pause))
                .clicked()
            {
                self.set_replay_paused(!self.replay_paused);
            }

            // Restart
            if ui.button("\u{23EE}").clicked() {
                self.restart_replay();
            }

            // Stop
            if ui.button("\u{23F9}").clicked() {
                stop_requested = true;
            }

            ui.separator();

            // Speed combo box — use id_salt for stable identity
            let selected_label = replay_speed_label(current_speed);
            egui::ComboBox::from_id_source("replay_speed_combo")
                .selected_text(format!("Speed: {selected_label}"))
                .show_ui(ui, |ui| {
                    for (speed, label) in REPLAY_SPEED_OPTIONS {
                        if ui
                            .selectable_label(replay_speed_matches(current_speed, speed), label)
                            .clicked()
                        {
                            new_speed = Some(speed);
                        }
                    }
                });

            ui.separator();

            // Measure time label width so the slider fills the rest.
            let time_width =
                ui.fonts(|f| {
                    f.layout_no_wrap(
                        time_text.clone(),
                        egui::FontId::default(),
                        egui::Color32::WHITE,
                    )
                })
                .size()
                .x + ui.spacing().item_spacing.x * 2.0;

            // Timeline slider — fill remaining width minus time label
            let mut timeline_fraction = self
                .replay_seek_drag_value
                .unwrap_or_else(|| self.current_replay_fraction());
            let slider = egui::Slider::new(&mut timeline_fraction, 0.0..=1.0).show_value(false);
            let slider_width = (ui.available_width() - time_width).max(80.0);
            let response = ui.add_sized([slider_width, ui.spacing().interact_size.y], slider);
            if response.dragged() {
                self.replay_seek_drag_value = Some(timeline_fraction);
            }
            if response.drag_stopped() || (response.changed() && !response.dragged()) {
                self.replay_seek_drag_value = None;
                self.seek_replay(timeline_fraction);
            }

            // Time label
            ui.label(&time_text);
        });

        if let Some(speed) = new_speed {
            self.set_replay_speed(speed);
        }
        if stop_requested {
            self.stop_pipeline();
        }
    }

    fn copy_detected_hotpixels_to_mask(&mut self) {
        use crate::settings::IMX636_DEM_SLOTS;

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
        let settings_locked = self.settings_are_locked();
        let mut plugin_toggle_changed = false;
        let mut view_mode_changed = false;
        let mut plugin_scan_requested = false;
        let mut open_plugins_dir_requested = false;
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                // ── File ──────────────────────────────────────────────────────
                ui.menu_button("File", |ui| {
                    if mode == AppMode::Replaying {
                        if let Some(path) = &self.replay_path {
                            let fname = PathBuf::from(path)
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone());
                            ui.add_enabled(
                                false,
                                egui::Label::new(
                                    egui::RichText::new(format!("Replaying: {fname}")).weak(),
                                ),
                            );
                        }
                        if ui.button("Close Replay").clicked() {
                            self.stop_pipeline();
                            ui.close_menu();
                        }
                        ui.separator();
                    } else {
                        let output_enabled = mode != AppMode::Recording;
                        ui.horizontal(|ui| {
                            ui.label("Output:");
                            ui.add_enabled(
                                output_enabled,
                                egui::TextEdit::singleline(&mut self.output_path)
                                    .desired_width(180.0),
                            );
                            if ui
                                .add_enabled(output_enabled, egui::Button::new("Browse…"))
                                .clicked()
                            {
                                let default_name =
                                    format!("output_{}.raw", format_timestamp_now());
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_file_name(&default_name)
                                    .add_filter("RAW", &["raw"])
                                    .save_file()
                                {
                                    self.output_path = path.display().to_string();
                                }
                                ui.close_menu();
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add_enabled(
                                output_enabled,
                                egui::Checkbox::new(
                                    &mut self.always_timestamp,
                                    "Always add timestamp",
                                ),
                            );
                        });
                        ui.separator();
                        if ui
                            .add_enabled(mode == AppMode::Idle, egui::Button::new("Open Replay…"))
                            .clicked()
                        {
                            self.open_replay_file();
                            ui.close_menu();
                        }
                        ui.separator();
                    }
                    if ui
                        .add_enabled(
                            mode != AppMode::Replaying,
                            egui::Button::new("Save Config…"),
                        )
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
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            mode != AppMode::Recording && mode != AppMode::Replaying,
                            egui::Button::new("Load Config…"),
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
                        ui.close_menu();
                    }
                });

                // ── Camera ────────────────────────────────────────────────────
                ui.menu_button("Camera", |ui| {
                    if ui
                        .add_enabled(mode == AppMode::Idle, egui::Button::new("Probe Camera"))
                        .clicked()
                    {
                        self.probe_camera();
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            mode == AppMode::Idle && self.camera_info.is_some(),
                            egui::Button::new("Preview"),
                        )
                        .clicked()
                    {
                        self.start_preview();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            mode == AppMode::Idle || mode == AppMode::Previewing,
                            egui::Button::new("Record"),
                        )
                        .clicked()
                    {
                        self.start_recording();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(mode != AppMode::Idle, egui::Button::new("Stop"))
                        .clicked()
                    {
                        self.stop_pipeline();
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            (mode == AppMode::Previewing || mode == AppMode::Recording)
                                && !settings_locked,
                            egui::Button::new("Apply Settings"),
                        )
                        .clicked()
                    {
                        self.apply_runtime_changes();
                        ui.close_menu();
                    }
                    if mode == AppMode::Replaying {
                        ui.separator();
                        let play_pause_label = if self.replay_paused { "Play" } else { "Pause" };
                        if ui
                            .add_enabled(!self.replay_finished, egui::Button::new(play_pause_label))
                            .clicked()
                        {
                            self.set_replay_paused(!self.replay_paused);
                            ui.close_menu();
                        }
                        if ui.button("Restart").clicked() {
                            self.restart_replay();
                            ui.close_menu();
                        }
                        ui.separator();
                        ui.label("Speed:");
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
                    } else {
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label("Acq time [ms]");
                            if ui
                                .add_enabled(
                                    !settings_locked,
                                    egui::Slider::new(&mut self.acq_time_ms, 1..=1000),
                                )
                                .changed()
                            {
                                self.acq_dirty = true;
                            }
                        });
                    }
                });

                // ── View ──────────────────────────────────────────────────────
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.settings_panel_open, "Settings Panel");
                    ui.checkbox(&mut self.analysis_panel_open, "Analysis Panel");
                    ui.separator();
                    let mut view_mode = self.preview_workspace.view_mode;
                    let r2d = ui.radio_value(&mut view_mode, ViewMode::Preview2d, "2D Preview");
                    let r3d =
                        ui.radio_value(&mut view_mode, ViewMode::PointCloud3d, "3D Point Cloud");
                    if r2d.changed() || r3d.changed() {
                        view_mode_changed = true;
                        self.preview_workspace.set_view_mode(view_mode);
                    }
                });

                // ── Plugins ───────────────────────────────────────────────────
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

                // ── Analysis ──────────────────────────────────────────────────
                let has_plugins = !self.builtin_plugins.is_empty()
                    || self
                        .plugin_manager
                        .records()
                        .iter()
                        .any(|r| r.plugin().is_some());
                if has_plugins {
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
                }

                // ── Right-aligned status area ────────────────────────────────
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(&self.camera_status);
                    ui.separator();
                    let mut view_mode = self.preview_workspace.view_mode;
                    let r3d = ui.selectable_value(&mut view_mode, ViewMode::PointCloud3d, "3D");
                    let r2d = ui.selectable_value(&mut view_mode, ViewMode::Preview2d, "2D");
                    if r2d.changed() || r3d.changed() {
                        view_mode_changed = true;
                        self.preview_workspace.set_view_mode(view_mode);
                    }
                    if mode == AppMode::Recording {
                        ui.separator();
                        ui.colored_label(egui::Color32::from_rgb(220, 50, 50), "● REC");
                    }
                    if mode == AppMode::Replaying && self.replay_finished {
                        ui.separator();
                        ui.colored_label(status_success_color(), "Finished");
                    }
                });
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
        }

        if settings_locked {
            self.preview_workspace.clear_selection();
        }

        if self.settings_panel_open {
            egui::SidePanel::left("settings")
                .min_width(340.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.heading("Settings");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("◀")
                                            .size(14.0)
                                            .color(ui.visuals().weak_text_color()),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                self.settings_panel_open = false;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            if mode != AppMode::Replaying {
                                ui.checkbox(
                                    &mut self.lock_settings_while_recording,
                                    "Lock settings while recording",
                                );
                            }

                            match mode {
                                AppMode::Recording if settings_locked => {
                                    ui.label("Recording: settings are locked.");
                                }
                                AppMode::Recording => {
                                    ui.label(
                                        "Recording: edits stay local until you click Apply Settings.",
                                    );
                                }
                                AppMode::Previewing => {
                                    ui.label(
                                        "Previewing: edits stay local until you click Apply Settings.",
                                    );
                                }
                                AppMode::Replaying => {
                                    ui.label(
                                        "Replay mode: camera settings are shown as read-only reference data.",
                                    );
                                    if let Some(notice) = &self.replay_notice {
                                        ui.colored_label(ui.visuals().warn_fg_color, notice);
                                    }
                                }
                                AppMode::Idle => {
                                    ui.label(
                                        "Idle: edits change the local config for the next recording.",
                                    );
                                }
                            }

                            ui.separator();
                            ui.add_enabled_ui(!settings_locked, |ui| {
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
                });
        } else {
            egui::SidePanel::left("settings-collapsed")
                .exact_width(COLLAPSED_PANEL_WIDTH)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("▶")
                                        .size(14.0)
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            self.settings_panel_open = true;
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
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("▶")
                                            .size(14.0)
                                            .color(ui.visuals().weak_text_color()),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                self.analysis_panel_open = false;
                            }
                        });
                        ui.heading("Analysis Tools");
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
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
                                        ui.visuals().warn_fg_color,
                                        format!(
                                            "Missing dependency: {}",
                                            missing_dependencies.join(", ")
                                        ),
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
                                        ui.visuals().warn_fg_color,
                                        format!(
                                            "Missing dependency: {}",
                                            missing_dependencies.join(", ")
                                        ),
                                    );
                                }

                                if let Err(err) = render_plugin_settings(ui, plugin) {
                                    ui.colored_label(
                                        ui.visuals().error_fg_color,
                                        format!("Dynamic plugin UI failed: {err}"),
                                    );
                                }

                                ui.separator();
                            }
                        });
                });
        } else if !enabled_plugin_names.is_empty() {
            egui::SidePanel::right("analysis-collapsed")
                .exact_width(COLLAPSED_PANEL_WIDTH)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.centered_and_justified(|ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("◀")
                                        .size(14.0)
                                        .color(ui.visuals().weak_text_color()),
                                )
                                .frame(false),
                            )
                            .clicked()
                        {
                            self.analysis_panel_open = true;
                        }
                    });
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
                                ui.visuals().warn_fg_color
                            } else {
                                status_success_color()
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
                                ui.colored_label(ui.visuals().warn_fg_color, error);
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
        }

        let mut pc_metrics: Option<PointCloudMetrics> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(preview_heading(mode, self.preview_workspace.view_mode));
            ui.separator();

            if let Some(info) = &self.camera_info {
                ui.label(format!(
                    "{} | serial: {} | firmware: {}",
                    info.compatible.as_deref().unwrap_or(&info.model),
                    info.serial.as_deref().unwrap_or("-"),
                    info.firmware.as_deref().unwrap_or("-"),
                ));
            }

            // Reserve space for controls below so the canvas never crowds them out.
            let controls_reserve = 190.0;

            if self.preview_workspace.popup_open {
                // When the popup is open, show a placeholder in the main window.
                draw_preview_toolbar(ui, &mut self.preview_workspace, settings_locked, "Enlarge");
                let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
                let placeholder_height = max_image_height;
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), placeholder_height),
                    egui::Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, PANEL_ROUNDING, ui.visuals().extreme_bg_color);
                painter.rect_stroke(
                    rect,
                    PANEL_ROUNDING,
                    egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                );
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Preview open in separate window",
                    egui::FontId::proportional(16.0),
                    ui.visuals().weak_text_color(),
                );
            } else {
                match self.preview_workspace.view_mode {
                    ViewMode::Preview2d => {
                        draw_preview_toolbar(
                            ui,
                            &mut self.preview_workspace,
                            settings_locked,
                            "Enlarge",
                        );
                        let max_image_height =
                            (ui.available_size().y - controls_reserve).max(180.0);
                        if let (Some(texture), Some(frame)) =
                            (self.texture.as_ref(), self.latest_frame.as_ref())
                        {
                            if draw_preview_canvas(
                                ui,
                                texture,
                                frame,
                                &mut self.config,
                                &mut self.preview_workspace,
                                settings_locked,
                                max_image_height,
                            ) && mode != AppMode::Replaying
                            {
                                self.config_dirty = true;
                            }
                        } else {
                            self.preview_workspace.hover_sensor = None;
                            draw_empty_preview_placeholder(
                                ui,
                                max_image_height,
                                mode,
                                self.preview_workspace.view_mode,
                            );
                        }
                    }
                    ViewMode::PointCloud3d => {
                        draw_point_cloud_toolbar(ui, &mut self.preview_workspace, "Enlarge");
                        let max_image_height =
                            (ui.available_size().y - controls_reserve).max(180.0);
                        pc_metrics = Some(self.preview_workspace.point_cloud.draw(
                            ui,
                            self.config.roi,
                            max_image_height,
                        ));
                    }
                }
            }

            if mode == AppMode::Replaying {
                ui.add_space(4.0);
                self.draw_replay_transport(ui);
            }

            // ── Scrollable controls below the canvas ─────────────────────────
            egui::ScrollArea::vertical()
                .max_height(controls_reserve)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.separator();
                    match self.preview_workspace.view_mode {
                        ViewMode::Preview2d => {
                            ui.horizontal(|ui| {
                                ui.label("Contrast");
                                ui.add(egui::Slider::new(
                                    &mut self.contrast_percentile,
                                    90.0..=100.0,
                                ));
                                ui.label(format!("{:.1}th percentile", self.contrast_percentile));
                            });
                        }
                        ViewMode::PointCloud3d => {
                            if let Some(metrics) = pc_metrics {
                                draw_point_cloud_metrics(ui, metrics);
                            }
                        }
                    }

                    if let Some(ctrl) = &self.controller {
                        let s = ctrl.stats_snapshot();
                        ui.label(format!(
                            "{:.2} Mev/s  |  {:.2} MB/s  |  {:02}:{:02}:{:02} elapsed",
                            s.mev_per_s,
                            s.mb_per_s,
                            (s.elapsed_s as u64) / 3600,
                            (s.elapsed_s as u64 % 3600) / 60,
                            s.elapsed_s as u64 % 60
                        ));
                    }
                    if let Some(frame) = &self.latest_frame {
                        let total = frame.on_count + frame.off_count;
                        if total > 0 {
                            let on_pct = frame.on_count as f64 * 100.0 / total as f64;
                            let off_pct = frame.off_count as f64 * 100.0 / total as f64;
                            ui.label(format!(
                                "ON {:.1}%  |  OFF {:.1}%  ({} ev this frame)",
                                on_pct, off_pct, total
                            ));
                        }
                    }

                    if mode != AppMode::Idle && mode != AppMode::Replaying {
                        if self.config_dirty || self.acq_dirty {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
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
                                analysis_warning_color(warning.severity, ui.visuals()),
                                format!("{}: {}", warning.source, warning.message),
                            );
                        }

                        let detected_pixels = self.latest_detected_hotpixels();
                        if !detected_pixels.is_empty() {
                            let can_copy = !settings_locked;
                            if ui
                                .add_enabled(can_copy, egui::Button::new("Mask detected hotpixels"))
                                .clicked()
                            {
                                self.copy_detected_hotpixels_to_mask();
                            }
                            if !can_copy {
                                ui.label(
                                    "Unlock runtime settings to copy detections into the DEM mask.",
                                );
                            }
                        }
                    }

                    if let Some(notice) = &self.analysis_notice {
                        ui.label(notice);
                    }

                    if let Some(err) = &self.last_error {
                        ui.separator();
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                });
        });

        if self.preview_workspace.popup_open {
            // Update shared data for the popup viewport.
            {
                let mut data = self.popup_shared.lock().unwrap();
                data.texture = self.texture.clone();
                data.mode = mode;
                data.view_mode = self.preview_workspace.view_mode;
                data.roi = self.config.roi;
                if let Some(frame) = &self.latest_frame {
                    data.frame_width = frame.width;
                    data.frame_height = frame.height;
                } else {
                    data.frame_width = 0;
                    data.frame_height = 0;
                }
                data.replay_paused = self.replay_paused;
                data.replay_finished = self.replay_finished;
                data.replay_speed = self.replay_speed;
                data.replay_fraction = self.current_replay_fraction();
                data.replay_time_us = self.current_replay_time_us();
                data.replay_bytes_read = self.current_replay_bytes_read();
                if let Some(info) = &self.replay_file_info {
                    data.replay_duration_us = info.total_duration_us;
                    data.replay_data_len = info.data_len();
                } else {
                    data.replay_duration_us = 0;
                    data.replay_data_len = 0;
                }
            }

            let shared = Arc::clone(&self.popup_shared);
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("popup_preview"),
                egui::ViewportBuilder::default()
                    .with_title("Preview \u{2014} AugurRS")
                    .with_inner_size([1280.0, 820.0]),
                move |ctx, class| match class {
                    egui::viewport::ViewportClass::Deferred => {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            render_popup_content(ui, &shared);
                        });
                        if ctx.input(|i| i.viewport().close_requested()) {
                            if let Ok(mut d) = shared.lock() {
                                d.close_requested = true;
                            }
                        }
                    }
                    egui::viewport::ViewportClass::Embedded => {
                        let mut open = true;
                        egui::Window::new("Preview \u{2014} AugurRS")
                            .open(&mut open)
                            .default_size([1100.0, 760.0])
                            .show(ctx, |ui| {
                                render_popup_content(ui, &shared);
                            });
                        if !open {
                            if let Ok(mut d) = shared.lock() {
                                d.close_requested = true;
                            }
                        }
                    }
                    _ => {}
                },
            );

            // Drain actions from the popup — extract values before dropping
            // the MutexGuard so we can call &mut self methods afterwards.
            let (close, toggle_pause, restart, seek_to, set_speed) = {
                let mut data = self.popup_shared.lock().unwrap();
                let close = data.close_requested;
                let toggle = data.toggle_pause;
                let restart = data.restart;
                let seek = data.seek_to.take();
                let speed = data.set_speed.take();
                data.close_requested = false;
                data.toggle_pause = false;
                data.restart = false;
                (close, toggle, restart, seek, speed)
            };
            if close {
                self.preview_workspace.popup_open = false;
            }
            if toggle_pause {
                let new_paused = !self.replay_paused;
                self.set_replay_paused(new_paused);
            }
            if restart {
                self.restart_replay();
            }
            if let Some(fraction) = seek_to {
                self.seek_replay(fraction);
            }
            if let Some(speed) = set_speed {
                self.set_replay_speed(speed);
            }
        }

        self.sync_active_pipeline_requirements();

        if self.mode != AppMode::Idle && self.controller.is_some() {
            ctx.request_repaint();
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewViewport {
    base_sensor_rect: egui::Rect,
    visible_sensor_rect: egui::Rect,
    /// ROI rect in sensor coordinates, if the config ROI is valid for this frame.
    roi_rect: Option<egui::Rect>,
}

impl PreviewViewport {
    fn display_size(&self, available: egui::Vec2) -> egui::Vec2 {
        let sensor_size = self.base_sensor_rect.size();
        let scale = (available.x / sensor_size.x)
            .min(available.y / sensor_size.y)
            .max(0.1);
        sensor_size * scale
    }

    fn uv_rect(&self, frame: &PreviewFrame) -> egui::Rect {
        let width = frame.width.max(1) as f32;
        let height = frame.height.max(1) as f32;
        egui::Rect::from_min_max(
            egui::pos2(
                self.visible_sensor_rect.min.x / width,
                self.visible_sensor_rect.min.y / height,
            ),
            egui::pos2(
                self.visible_sensor_rect.max.x / width,
                self.visible_sensor_rect.max.y / height,
            ),
        )
    }

    fn screen_to_sensor(
        &self,
        image_rect: egui::Rect,
        pointer_pos: egui::Pos2,
    ) -> Option<(u16, u16)> {
        if !image_rect.contains(pointer_pos) {
            return None;
        }

        let rel_x = ((pointer_pos.x - image_rect.min.x) / image_rect.width()).clamp(0.0, 1.0);
        let rel_y = ((pointer_pos.y - image_rect.min.y) / image_rect.height()).clamp(0.0, 1.0);
        let sensor_x = (self.visible_sensor_rect.min.x + rel_x * self.visible_sensor_rect.width())
            .floor()
            .max(0.0) as u16;
        let sensor_y = (self.visible_sensor_rect.min.y + rel_y * self.visible_sensor_rect.height())
            .floor()
            .max(0.0) as u16;
        Some((sensor_x, sensor_y))
    }

    fn sensor_to_screen(&self, image_rect: egui::Rect, sensor_pos: egui::Pos2) -> egui::Pos2 {
        let rel_x =
            (sensor_pos.x - self.visible_sensor_rect.min.x) / self.visible_sensor_rect.width();
        let rel_y =
            (sensor_pos.y - self.visible_sensor_rect.min.y) / self.visible_sensor_rect.height();
        egui::pos2(
            image_rect.min.x + rel_x * image_rect.width(),
            image_rect.min.y + rel_y * image_rect.height(),
        )
    }
}

fn preview_heading(mode: AppMode, view_mode: ViewMode) -> &'static str {
    match (mode, view_mode) {
        (AppMode::Replaying, ViewMode::Preview2d) => "Replay",
        (AppMode::Replaying, ViewMode::PointCloud3d) => "Replay 3D View",
        (_, ViewMode::Preview2d) => "Live Preview",
        (_, ViewMode::PointCloud3d) => "3D Point Cloud",
    }
}

fn empty_preview_message(mode: AppMode, view_mode: ViewMode) -> &'static str {
    match (mode, view_mode) {
        (AppMode::Idle, ViewMode::Preview2d) => {
            "No camera probed. Click Probe Camera to connect, or open a replay file."
        }
        (AppMode::Replaying, ViewMode::Preview2d) => {
            "No replay frame yet. Use the timeline or wait for playback to decode the next frame."
        }
        (_, ViewMode::Preview2d) => {
            "No preview yet. Probe the camera, then click Preview or Record."
        }
        (_, ViewMode::PointCloud3d) => "No recent raw events available for the 3D view yet.",
    }
}

fn draw_preview_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut PreviewWorkspaceState,
    settings_locked: bool,
    popup_button_label: &str,
) {
    ui.horizontal_wrapped(|ui| {
        let mut select_roi_button = ui.add_enabled(
            !settings_locked,
            egui::SelectableLabel::new(workspace.tool == PreviewTool::SelectRoi, "Select ROI"),
        );
        if settings_locked {
            select_roi_button = select_roi_button.on_hover_text(
                "ROI editing is disabled while replay is active or settings are locked.",
            );
        }
        if select_roi_button.clicked() {
            if workspace.tool == PreviewTool::SelectRoi {
                workspace.clear_selection();
            } else {
                workspace.tool = PreviewTool::SelectRoi;
                workspace.selection_anchor = None;
                workspace.pending_roi = None;
            }
        }

        if ui.button("-").clicked() {
            workspace.zoom = (workspace.zoom / 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
            if (workspace.zoom - PREVIEW_ZOOM_MIN).abs() < f32::EPSILON {
                workspace.pan = egui::Vec2::ZERO;
            }
        }
        if ui.button("+").clicked() {
            workspace.zoom = (workspace.zoom * 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
        }
        if ui.button("Fit").clicked() {
            workspace.reset_zoom();
        }

        ui.checkbox(&mut workspace.crop_to_roi, "Crop to ROI");

        if ui.button(popup_button_label).clicked() {
            workspace.popup_open = !workspace.popup_open;
        }

        ui.separator();
        if let Some((x, y)) = workspace.hover_sensor {
            ui.monospace(format!("x {x}, y {y}"));
        } else {
            ui.weak("Hover preview for x/y");
        }
    });
}

fn draw_point_cloud_toolbar(
    ui: &mut egui::Ui,
    workspace: &mut PreviewWorkspaceState,
    popup_button_label: &str,
) {
    workspace.point_cloud.sanitize_controls();

    egui::Grid::new("pc_controls_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Time range [ms]")
                .on_hover_text("How far back in time to show events");
            ui.add(
                egui::DragValue::new(&mut workspace.point_cloud.time_window_ms)
                    .speed(5.0)
                    .clamp_range(5.0..=2_000.0),
            );
            ui.end_row();

            ui.label("Max render")
                .on_hover_text("Limits rendered points for smoother interaction");
            ui.add(
                egui::DragValue::new(&mut workspace.point_cloud.point_limit)
                    .speed(250.0)
                    .clamp_range(1_000..=100_000),
            );
            ui.end_row();
        });

    ui.horizontal(|ui| {
        if ui.button("Reset Camera").clicked() {
            workspace.point_cloud.reset_camera();
        }
        if ui.button(popup_button_label).clicked() {
            workspace.popup_open = !workspace.popup_open;
        }
        ui.small("Drag to orbit. Scroll to zoom.");
    });
}

fn draw_point_cloud_metrics(ui: &mut egui::Ui, metrics: PointCloudMetrics) {
    if metrics.visible_points == 0 {
        ui.label("No events in view");
    } else if metrics.rendered_points < metrics.visible_points {
        ui.label(format!(
            "Showing {} of {} events (downsampled)",
            metrics.rendered_points, metrics.visible_points
        ));
    } else {
        ui.label(format!("Showing {} events", metrics.rendered_points));
    }
}

fn draw_empty_preview_placeholder(
    ui: &mut egui::Ui,
    max_image_height: f32,
    mode: AppMode,
    view_mode: ViewMode,
) {
    let placeholder_height = max_image_height;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), placeholder_height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, PANEL_ROUNDING, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        PANEL_ROUNDING,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        empty_preview_message(mode, view_mode),
        egui::FontId::proportional(16.0),
        ui.visuals().weak_text_color(),
    );
}

fn render_popup_content(ui: &mut egui::Ui, shared: &Arc<Mutex<PopupSharedData>>) {
    let data = shared.lock().unwrap();

    // Extract everything we need, then drop the lock before rendering.
    let texture = data.texture.clone();
    let view_mode = data.view_mode;
    let mode = data.mode;
    let is_replay = mode == AppMode::Replaying && data.replay_duration_us > 0;
    let fraction = data.seek_drag.unwrap_or(data.replay_fraction);
    let paused = data.replay_paused;
    let finished = data.replay_finished;
    let speed = data.replay_speed;
    let duration_us = data.replay_duration_us;
    let time_us = data.replay_time_us;
    let bytes_read = data.replay_bytes_read;
    let data_len = data.replay_data_len;
    drop(data);

    match view_mode {
        ViewMode::Preview2d => {
            if let Some(texture) = &texture {
                let reserve = if is_replay { 80.0 } else { 0.0 };
                let available = ui.available_size();
                let max_h = (available.y - reserve).max(100.0);
                ui.add(
                    egui::Image::new(texture)
                        .shrink_to_fit()
                        .max_size(egui::vec2(available.x, max_h)),
                );
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("No preview available").weak().italics());
                });
            }
        }
        ViewMode::PointCloud3d => {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("3D view available in main window")
                        .weak()
                        .italics(),
                );
            });
        }
    }

    // Replay controls
    if is_replay {
        ui.separator();
        ui.horizontal(|ui| {
            let play_label = if paused { "Play" } else { "Pause" };
            if ui
                .add_enabled(!finished, egui::Button::new(play_label))
                .clicked()
            {
                shared.lock().unwrap().toggle_pause = true;
            }
            if ui.button("Restart").clicked() {
                shared.lock().unwrap().restart = true;
            }
            ui.label("Speed:");
            ui.label(format!(
                "{:.1}x",
                if speed.is_infinite() {
                    f32::INFINITY
                } else {
                    speed
                }
            ));
        });

        let mut timeline = fraction;
        let slider = egui::Slider::new(&mut timeline, 0.0..=1.0)
            .show_value(false)
            .text("Timeline");
        let response = ui.add_sized([ui.available_width().max(300.0), 0.0], slider);
        if response.dragged() {
            shared.lock().unwrap().seek_drag = Some(timeline);
        }
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            let mut d = shared.lock().unwrap();
            d.seek_drag = None;
            d.seek_to = Some(timeline);
        }

        ui.horizontal(|ui| {
            ui.label(format!(
                "{} / {}",
                format_replay_time(time_us),
                format_replay_time(duration_us)
            ));
            ui.separator();
            ui.label(format!(
                "{:.1} / {:.1} MB",
                bytes_read as f64 / (1024.0 * 1024.0),
                data_len as f64 / (1024.0 * 1024.0)
            ));
        });
    }
}

fn draw_preview_canvas(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    frame: &PreviewFrame,
    config: &mut CameraConfig,
    workspace: &mut PreviewWorkspaceState,
    settings_locked: bool,
    max_height: f32,
) -> bool {
    let viewport = build_preview_viewport(frame, config, workspace);
    let available = ui.available_size_before_wrap();
    let display_size = viewport.display_size(egui::vec2(available.x, available.y.min(max_height)));
    let response = ui.add(
        egui::Image::new(texture)
            .uv(viewport.uv_rect(frame))
            .fit_to_exact_size(display_size)
            .sense(egui::Sense::click_and_drag()),
    );

    workspace.hover_sensor = response
        .hover_pos()
        .and_then(|pos| viewport.screen_to_sensor(response.rect, pos));

    let mut roi_committed = false;
    if workspace.tool == PreviewTool::SelectRoi && !settings_locked {
        if response.drag_started() {
            if let Some(pointer_pos) = response
                .interact_pointer_pos()
                .and_then(|pos| viewport.screen_to_sensor(response.rect, pos))
            {
                let anchor = egui::pos2(pointer_pos.0 as f32, pointer_pos.1 as f32);
                workspace.selection_anchor = Some(anchor);
                workspace.pending_roi = Some(sensor_rect_from_points(anchor, anchor, frame));
            }
        }

        if response.dragged() {
            if let (Some(anchor), Some(pointer_pos)) = (
                workspace.selection_anchor,
                response
                    .interact_pointer_pos()
                    .and_then(|pos| viewport.screen_to_sensor(response.rect, pos)),
            ) {
                workspace.pending_roi = Some(sensor_rect_from_points(
                    anchor,
                    egui::pos2(pointer_pos.0 as f32, pointer_pos.1 as f32),
                    frame,
                ));
            }
        }

        if response.drag_stopped() {
            if let Some(pending_roi) = workspace.pending_roi.take() {
                if let Some(roi) = sensor_rect_to_roi_config(pending_roi, frame) {
                    config.roi = roi;
                    roi_committed = true;
                    if workspace.crop_to_roi {
                        workspace.reset_zoom();
                    }
                }
            }
            workspace.selection_anchor = None;
        }
    } else if response.dragged() && workspace.zoom > PREVIEW_ZOOM_MIN {
        let delta = ui.ctx().input(|input| input.pointer.delta());
        workspace.pan += egui::vec2(
            -delta.x * viewport.visible_sensor_rect.width() / response.rect.width().max(1.0),
            -delta.y * viewport.visible_sensor_rect.height() / response.rect.height().max(1.0),
        );
    }

    let painter = ui.painter().with_clip_rect(response.rect);
    if let Some(current_roi) = viewport.roi_rect {
        if !workspace.crop_to_roi {
            paint_sensor_rect(
                &painter,
                response.rect,
                viewport,
                current_roi,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 196, 64)),
            );
        }
    }
    if let Some(pending_roi) = workspace.pending_roi {
        paint_sensor_rect(
            &painter,
            response.rect,
            viewport,
            pending_roi,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
        );
    }

    roi_committed
}

fn build_preview_viewport(
    frame: &PreviewFrame,
    config: &CameraConfig,
    workspace: &mut PreviewWorkspaceState,
) -> PreviewViewport {
    workspace.zoom = workspace.zoom.clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);

    let full_sensor_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(frame.width as f32, frame.height as f32),
    );
    let roi_rect = roi_sensor_rect(config, frame);
    let base_sensor_rect = if workspace.crop_to_roi {
        roi_rect.unwrap_or(full_sensor_rect)
    } else {
        full_sensor_rect
    };

    let visible_size = egui::vec2(
        (base_sensor_rect.width() / workspace.zoom).max(1.0),
        (base_sensor_rect.height() / workspace.zoom).max(1.0),
    );
    let max_pan = egui::vec2(
        ((base_sensor_rect.width() - visible_size.x) * 0.5).max(0.0),
        ((base_sensor_rect.height() - visible_size.y) * 0.5).max(0.0),
    );
    workspace.pan.x = workspace.pan.x.clamp(-max_pan.x, max_pan.x);
    workspace.pan.y = workspace.pan.y.clamp(-max_pan.y, max_pan.y);

    let center = base_sensor_rect.center() + workspace.pan;
    let min = egui::pos2(
        (center.x - visible_size.x * 0.5).clamp(
            base_sensor_rect.min.x,
            base_sensor_rect.max.x - visible_size.x,
        ),
        (center.y - visible_size.y * 0.5).clamp(
            base_sensor_rect.min.y,
            base_sensor_rect.max.y - visible_size.y,
        ),
    );

    PreviewViewport {
        base_sensor_rect,
        visible_sensor_rect: egui::Rect::from_min_size(min, visible_size),
        roi_rect,
    }
}

fn roi_sensor_rect(config: &CameraConfig, frame: &PreviewFrame) -> Option<egui::Rect> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }

    let min_x = config.roi.x.min(frame.width.saturating_sub(1)) as f32;
    let min_y = config.roi.y.min(frame.height.saturating_sub(1)) as f32;
    let max_x =
        (u32::from(config.roi.x) + u32::from(config.roi.width)).min(u32::from(frame.width)) as f32;
    let max_y = (u32::from(config.roi.y) + u32::from(config.roi.height))
        .min(u32::from(frame.height)) as f32;
    if max_x <= min_x || max_y <= min_y {
        return None;
    }

    Some(egui::Rect::from_min_max(
        egui::pos2(min_x, min_y),
        egui::pos2(max_x, max_y),
    ))
}

fn sensor_rect_from_points(start: egui::Pos2, end: egui::Pos2, frame: &PreviewFrame) -> egui::Rect {
    let max_x = frame.width.max(1) as f32;
    let max_y = frame.height.max(1) as f32;
    let min_x = start.x.min(end.x).floor().clamp(0.0, max_x - 1.0);
    let min_y = start.y.min(end.y).floor().clamp(0.0, max_y - 1.0);
    let max_x_rect = start.x.max(end.x).ceil().clamp(min_x + 1.0, max_x);
    let max_y_rect = start.y.max(end.y).ceil().clamp(min_y + 1.0, max_y);
    egui::Rect::from_min_max(egui::pos2(min_x, min_y), egui::pos2(max_x_rect, max_y_rect))
}

fn sensor_rect_to_roi_config(sensor_rect: egui::Rect, frame: &PreviewFrame) -> Option<RoiConfig> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }

    let min_x = sensor_rect
        .min
        .x
        .floor()
        .clamp(0.0, frame.width as f32 - 1.0) as u16;
    let min_y = sensor_rect
        .min
        .y
        .floor()
        .clamp(0.0, frame.height as f32 - 1.0) as u16;
    let max_x = sensor_rect
        .max
        .x
        .ceil()
        .clamp(min_x as f32 + 1.0, frame.width as f32) as u16;
    let max_y = sensor_rect
        .max
        .y
        .ceil()
        .clamp(min_y as f32 + 1.0, frame.height as f32) as u16;

    Some(RoiConfig {
        x: min_x,
        y: min_y,
        width: max_x.saturating_sub(min_x).max(1),
        height: max_y.saturating_sub(min_y).max(1),
    })
}

fn paint_sensor_rect(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    sensor_rect: egui::Rect,
    stroke: egui::Stroke,
) {
    let screen_rect = egui::Rect::from_min_max(
        viewport.sensor_to_screen(image_rect, sensor_rect.min),
        viewport.sensor_to_screen(image_rect, sensor_rect.max),
    );
    painter.rect_stroke(screen_rect, 0.0, stroke);
}

fn status_success_color() -> egui::Color32 {
    egui::Color32::from_rgb(0, 160, 60)
}

fn analysis_info_color() -> egui::Color32 {
    egui::Color32::from_rgb(30, 100, 220)
}

fn analysis_warning_color(severity: AnalysisSeverity, visuals: &egui::Visuals) -> egui::Color32 {
    match severity {
        AnalysisSeverity::Info => analysis_info_color(),
        AnalysisSeverity::Warning => visuals.warn_fg_color,
        AnalysisSeverity::Error => visuals.error_fg_color,
    }
}

fn replay_pipeline_config(info: &ReplayFileInfo) -> CameraConfig {
    let mut config = CameraConfig::default();
    config.roi.width = info.width;
    config.roi.height = info.height;
    config
}

fn replay_file_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

fn replay_config_path(raw_path: &Path) -> Option<PathBuf> {
    let stem = raw_path.file_stem()?.to_string_lossy();
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    Some(parent.join(format!("{stem}.toml")))
}

fn replay_speed_label(speed: f32) -> &'static str {
    for &(s, label) in &REPLAY_SPEED_OPTIONS {
        if replay_speed_matches(speed, s) {
            return label;
        }
    }
    "1x"
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

#[cfg(test)]
mod tests {
    use super::{
        build_preview_viewport, sensor_rect_to_roi_config, PreviewWorkspaceState, PREVIEW_ZOOM_MAX,
    };
    use augur_core::{config::CameraConfig, pipeline::PreviewFrame};

    fn test_frame() -> PreviewFrame {
        PreviewFrame {
            width: 1280,
            height: 720,
            pixels: vec![0; 1280 * 720],
            pixels_on: vec![0; 1280 * 720],
            pixels_off: vec![0; 1280 * 720],
            on_count: 0,
            off_count: 0,
            events: None,
            window_start_us: 0,
            window_end_us: 0,
        }
    }

    #[test]
    fn preview_viewport_uses_roi_when_crop_is_enabled() {
        let frame = test_frame();
        let mut config = CameraConfig::default();
        config.roi.x = 120;
        config.roi.y = 90;
        config.roi.width = 320;
        config.roi.height = 180;

        let mut workspace = PreviewWorkspaceState {
            crop_to_roi: true,
            ..Default::default()
        };
        let viewport = build_preview_viewport(&frame, &config, &mut workspace);

        assert_eq!(viewport.base_sensor_rect.min, egui::pos2(120.0, 90.0));
        assert_eq!(viewport.base_sensor_rect.size(), egui::vec2(320.0, 180.0));
    }

    #[test]
    fn preview_viewport_clamps_pan_inside_base_rect() {
        let frame = test_frame();
        let config = CameraConfig::default();
        let mut workspace = PreviewWorkspaceState {
            zoom: PREVIEW_ZOOM_MAX,
            pan: egui::vec2(10_000.0, -10_000.0),
            ..Default::default()
        };

        let viewport = build_preview_viewport(&frame, &config, &mut workspace);

        assert!(viewport.visible_sensor_rect.min.x >= 0.0);
        assert!(viewport.visible_sensor_rect.min.y >= 0.0);
        assert!(viewport.visible_sensor_rect.max.x <= frame.width as f32);
        assert!(viewport.visible_sensor_rect.max.y <= frame.height as f32);
    }

    #[test]
    fn roi_conversion_rounds_selection_to_sensor_bounds() {
        let frame = test_frame();
        let rect = egui::Rect::from_min_max(egui::pos2(10.2, 20.1), egui::pos2(25.8, 30.0));
        let roi = sensor_rect_to_roi_config(rect, &frame).expect("roi should be produced");

        assert_eq!(roi.x, 10);
        assert_eq!(roi.y, 20);
        assert_eq!(roi.width, 16);
        assert_eq!(roi.height, 10);
    }
}
