use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime},
};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisWarning, Overlay},
    camera::{DeviceInfo, EventCamera},
    config::{CameraConfig, GlobalSettingsConfig},
    metadata::{RecordingAnnotations, RecordingMetadata},
    pipeline::{
        spawn_pipeline, CdEvent, Evt3CorePreviewDecoder, PipelineController, PipelineOptions,
        PipelineStatsSnapshot, PreviewFrame,
    },
    replay::{align_relative_evt3_word_offset, RawFileCamera, ReplayControls, ReplayFileInfo},
    DecodedEventFileCamera, PackedEventPreviewDecoder, PACKED_EVENT_RECORD_BYTES,
};
use augur_plugin_api::{
    EventStore, FfiCdEvent, GlobalSettings, HostDatasetKind, HostViewKind, Image2dV1, PluginInput,
    Series1dV1, TableDatasetV1, CTX_GLOBAL_SETTINGS,
};
use augur_prophesee::evk4::Evk4Camera;

use crate::{
    export::{export_tiff_stack, ExportEventSource, TiffStackExportParams},
    export_dialog::{ExportDialog, ExportDialogAction},
    external_tools::{
        ExternalTool, ExternalToolStatus, ImageJBridge, BUNDLED_IMAGEJ_PLUGIN_JAR,
        BUNDLED_IMAGEJ_PLUGIN_JAR_NAME, DEFAULT_IMAGEJ_BRIDGE_PORT,
    },
    host_views::{
        decode_dataset_snapshot, export_image_to_path, export_table_csv_to_path,
        render_compact_table, render_density2d_view, render_density_window_viewport,
        render_image2d_view, render_image_window_viewport, render_line_series_view,
        render_scatter2d_view, render_scatter_window_viewport, render_series_window_viewport,
        render_table_window, render_table_window_viewport, reset_provider_for_dataset,
        resolve_host_view_registry, DensityWindowViewportData, HostDatasetSnapshot,
        HostRegistryContribution, HostViewImageFormat, HostViewProviderKey, HostViewRenderState,
        HostViewUiActions, ImageWindowViewportData, ResolvedHostView, ResolvedHostViewRegistry,
        Scatter2dViewOptions, ScatterWindowViewportData, SeriesWindowViewportData,
        TableWindowViewportData,
    },
    plugin::AnalysisPlugin,
    plugin_loader::PluginManager,
    plugin_settings_ui::render_plugin_settings,
    plugins::create_all_plugins,
    preview::{
        compute_frame_histogram, frame_to_color_image, reset_preview_render_cache,
        PreviewDisplaySettings,
    },
    settings::draw_settings,
    viewer_widget::{
        draw_text_placeholder, draw_viewer, AppMode, PreviewTool, ViewMode, ViewerInput,
        ViewerOutput, ViewerReplayState, ViewerState,
    },
};

const COLLAPSED_PANEL_WIDTH: f32 = 22.0;
const EVENT_STORE_MEBIBYTE: usize = 1024 * 1024;
pub(crate) const PANEL_ROUNDING: f32 = 6.0;
const REPLAY_SPEED_OPTIONS: [(f32, &str); 6] = [
    (0.25, "0.25x"),
    (0.5, "0.5x"),
    (1.0, "1x"),
    (2.0, "2x"),
    (4.0, "4x"),
    (f32::INFINITY, "Max"),
];

type CachedHostDataset = Result<Option<HostDatasetSnapshot>, String>;

fn process_interval_for_view_mode(
    view_mode: ViewMode,
    preview_interval_ms: u64,
    point_cloud_interval_ms: u64,
) -> Duration {
    if view_mode == ViewMode::PointCloud3d {
        Duration::from_millis(point_cloud_interval_ms)
    } else {
        Duration::from_millis(preview_interval_ms)
    }
}

fn viewport_stream_active(mode: AppMode, replay: ViewerReplayState) -> bool {
    matches!(mode, AppMode::Previewing | AppMode::Recording)
        || (mode == AppMode::Replaying && replay.active && !replay.paused && !replay.finished)
}

fn child_viewport_repaint_after(
    stream_active: bool,
    replay_open_task_active: bool,
    process_interval: Duration,
    preview_interval_ms: u64,
) -> Option<Duration> {
    let mut repaint_after =
        replay_open_task_active.then_some(Duration::from_millis(preview_interval_ms));
    if stream_active {
        repaint_after = Some(
            repaint_after
                .map(|existing| existing.min(process_interval))
                .unwrap_or(process_interval),
        );
    }
    repaint_after
}

fn request_root_repaint(ctx: &egui::Context) {
    ctx.request_repaint_of(egui::ViewportId::ROOT);
}

fn mib_to_bytes(mib: u64) -> usize {
    mib.saturating_mul(EVENT_STORE_MEBIBYTE as u64)
        .min(usize::MAX as u64) as usize
}

fn interval_ms_to_hz(interval_ms: u64) -> f64 {
    1000.0 / interval_ms.max(1) as f64
}

fn hz_to_interval_ms(hz: f64, hz_range: std::ops::RangeInclusive<f64>) -> u64 {
    let hz = hz.clamp(*hz_range.start(), *hz_range.end());
    (1000.0 / hz).round().max(1.0) as u64
}

fn derived_replay_preview_interval_ms(acq_time_ms: u64, speed: f32) -> u64 {
    if speed.is_finite() && speed > 0.0 {
        (acq_time_ms as f64 / speed as f64).round() as u64
    } else {
        10
    }
    .clamp(10, 200)
}

#[derive(Debug, Clone)]
struct HostDatasetCacheEntry {
    generation: u64,
    snapshot: CachedHostDataset,
}

/// Extract a table reference from a cached dataset entry.
/// Returns `Ok(Some(table))` if data is available, `Ok(None)` if absent, or
/// `Err(message)` if loading failed.
fn cached_table(entry: Option<&HostDatasetCacheEntry>) -> Result<Option<&TableDatasetV1>, &str> {
    match entry {
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Table(table))),
            ..
        }) => Ok(Some(table.as_ref())),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(None), ..
        })
        | None => Ok(None),
        Some(HostDatasetCacheEntry {
            snapshot: Err(err), ..
        }) => Err(err.as_str()),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Image2d(_) | HostDatasetSnapshot::Series1d(_))),
            ..
        }) => Err("cached dataset is not a table dataset"),
    }
}

fn cached_image(entry: Option<&HostDatasetCacheEntry>) -> Result<Option<&Image2dV1>, &str> {
    match entry {
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Image2d(image))),
            ..
        }) => Ok(Some(image.as_ref())),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(None), ..
        })
        | None => Ok(None),
        Some(HostDatasetCacheEntry {
            snapshot: Err(err), ..
        }) => Err(err.as_str()),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Table(_) | HostDatasetSnapshot::Series1d(_))),
            ..
        }) => Err("cached dataset is not an image dataset"),
    }
}

fn cached_series(entry: Option<&HostDatasetCacheEntry>) -> Result<Option<&Series1dV1>, &str> {
    match entry {
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Series1d(series))),
            ..
        }) => Ok(Some(series.as_ref())),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(None), ..
        })
        | None => Ok(None),
        Some(HostDatasetCacheEntry {
            snapshot: Err(err), ..
        }) => Err(err.as_str()),
        Some(HostDatasetCacheEntry {
            snapshot: Ok(Some(HostDatasetSnapshot::Table(_) | HostDatasetSnapshot::Image2d(_))),
            ..
        }) => Err("cached dataset is not a line-series dataset"),
    }
}

#[derive(Debug, Clone)]
struct SavedLiveState {
    config: CameraConfig,
    mask_file: String,
    camera_info: Option<DeviceInfo>,
}

struct OpenedReplay {
    controller: PipelineController,
    controls: ReplayControls,
    info: ReplayFileInfo,
    replay_info: DeviceInfo,
    decoded_events: Option<Arc<Vec<CdEvent>>>,
}

struct ReplayOpenTask {
    path: PathBuf,
    saved_live_state: SavedLiveState,
    rx: mpsc::Receiver<Result<OpenedReplay, String>>,
}

struct TiffStackExportTask {
    output_path: PathBuf,
    rx: mpsc::Receiver<Result<usize, String>>,
}

struct PopupSharedData {
    viewer: ViewerState,
    texture: Option<egui::TextureHandle>,
    frame: Option<PreviewFrame>,
    overlays: Vec<Overlay>,
    camera_info: Option<DeviceInfo>,
    nm_per_pixel: f64,
    config: CameraConfig,
    mode: AppMode,
    settings_locked: bool,
    pipeline_stats: Option<PipelineStatsSnapshot>,
    replay: ViewerReplayState,
    analysis_warnings: Vec<AnalysisWarning>,
    analysis_notice: Option<String>,
    detected_hotpixels: Vec<(u16, u16)>,
    config_dirty: bool,
    acq_dirty: bool,
    replay_open_task_active: bool,
    replay_notice: Option<String>,
    last_error: Option<String>,
    external_streaming: bool,
    external_streaming_label: String,
    preview_interval_ms: u64,
    point_cloud_interval_ms: u64,
    close_requested: bool,
    output: Option<ViewerOutput>,
}

impl Default for PopupSharedData {
    fn default() -> Self {
        let global_defaults = GlobalSettingsConfig::default();
        Self {
            viewer: ViewerState::default(),
            texture: None,
            frame: None,
            overlays: Vec::new(),
            camera_info: None,
            nm_per_pixel: 1.0,
            config: CameraConfig::default(),
            mode: AppMode::Idle,
            settings_locked: false,
            pipeline_stats: None,
            replay: ViewerReplayState::default(),
            analysis_warnings: Vec::new(),
            analysis_notice: None,
            detected_hotpixels: Vec::new(),
            config_dirty: false,
            acq_dirty: false,
            replay_open_task_active: false,
            replay_notice: None,
            last_error: None,
            external_streaming: false,
            external_streaming_label: String::new(),
            preview_interval_ms: global_defaults.preview_interval_ms,
            point_cloud_interval_ms: global_defaults.point_cloud_interval_ms,
            close_requested: false,
            output: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ImageJDialogState {
    open: bool,
    host: String,
    port: u16,
    error: Option<String>,
    info: Option<String>,
}

impl Default for ImageJDialogState {
    fn default() -> Self {
        Self {
            open: false,
            host: "127.0.0.1".into(),
            port: DEFAULT_IMAGEJ_BRIDGE_PORT,
            error: None,
            info: None,
        }
    }
}

fn format_timestamp_now() -> String {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs();
    let secs_per_day: u64 = 86400;
    let days = total_secs / secs_per_day;
    let day_secs = total_secs % secs_per_day;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}{mo:02}{d:02}_{h:02}{m:02}{s:02}")
}

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
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("raw");
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
    last_preview_process_at: Option<Instant>,
    replay_controls: Option<ReplayControls>,
    replay_file_info: Option<ReplayFileInfo>,
    replay_decoded_events: Option<Arc<Vec<CdEvent>>>,
    replay_paused: bool,
    replay_finished: bool,
    replay_pause_after_seek_frame: bool,
    replay_speed: f32,
    replay_notice: Option<String>,
    replay_open_task: Option<ReplayOpenTask>,
    saved_live_state: Option<SavedLiveState>,
    builtin_plugins: Vec<Box<dyn AnalysisPlugin>>,
    plugin_manager: PluginManager,
    plugin_context_data: HashMap<String, Vec<u8>>,
    persistent_context_data: HashMap<String, Vec<u8>>,
    event_store: EventStore,
    nm_per_pixel: f64,
    sensor_width: u16,
    sensor_height: u16,
    analysis_output: AnalysisOutput,
    analysis_notice: Option<String>,
    acq_time_ms: u64,
    preview_interval_ms: u64,
    point_cloud_interval_ms: u64,
    disk_writer_buffer_mib: u64,
    acq_dirty: bool,
    config_dirty: bool,
    viewer: ViewerState,
    popup_open: bool,
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
    imagej_dialog: ImageJDialogState,
    export_dialog: ExportDialog,
    export_task: Option<TiffStackExportTask>,
    external_tool: Option<Box<dyn ExternalTool>>,
    host_view_registry: ResolvedHostViewRegistry,
    host_view_registry_dirty: bool,
    host_view_window_open: HashMap<String, bool>,
    host_view_render_state: HashMap<String, HostViewRenderState>,
    host_view_dataset_cache: HashMap<String, HostDatasetCacheEntry>,
    host_view_resolution_warnings: Vec<String>,
}

impl CameraApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);

        let mut plugin_manager = PluginManager::new_default();
        let plugin_scan_error = plugin_manager.scan_and_load().err();
        let global_defaults = GlobalSettingsConfig::default();

        let mut app = Self {
            config: CameraConfig::default(),
            output_path: format!("./output_{}.raw", format_timestamp_now()),
            always_timestamp: false,
            replay_path: None,
            mode: AppMode::Idle,
            controller: None,
            texture: None,
            latest_frame: None,
            last_preview_process_at: None,
            replay_controls: None,
            replay_file_info: None,
            replay_decoded_events: None,
            replay_paused: false,
            replay_finished: false,
            replay_pause_after_seek_frame: false,
            replay_speed: 1.0,
            replay_notice: None,
            replay_open_task: None,
            saved_live_state: None,
            builtin_plugins: create_all_plugins(),
            plugin_manager,
            plugin_context_data: HashMap::new(),
            persistent_context_data: HashMap::new(),
            event_store: EventStore::default(),
            nm_per_pixel: global_defaults.nm_per_pixel,
            sensor_width: global_defaults.sensor_width,
            sensor_height: global_defaults.sensor_height,
            analysis_output: AnalysisOutput::default(),
            analysis_notice: None,
            acq_time_ms: global_defaults.acq_time_ms,
            preview_interval_ms: global_defaults.preview_interval_ms,
            point_cloud_interval_ms: global_defaults.point_cloud_interval_ms,
            disk_writer_buffer_mib: global_defaults.disk_writer_buffer_mib,
            acq_dirty: false,
            config_dirty: false,
            viewer: ViewerState::default(),
            popup_open: false,
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
            imagej_dialog: ImageJDialogState::default(),
            export_dialog: ExportDialog::default(),
            export_task: None,
            external_tool: None,
            host_view_registry: ResolvedHostViewRegistry::default(),
            host_view_registry_dirty: true,
            host_view_window_open: HashMap::new(),
            host_view_render_state: HashMap::new(),
            host_view_dataset_cache: HashMap::new(),
            host_view_resolution_warnings: Vec::new(),
        };
        app.event_store
            .set_memory_budget(mib_to_bytes(global_defaults.event_store_budget_mib));
        app.sync_config_global_from_runtime();
        app.refresh_host_view_registry();
        app
    }

    fn event_store_budget_mib(&self) -> u64 {
        (self.event_store.memory_budget_bytes() / EVENT_STORE_MEBIBYTE).max(1) as u64
    }

    fn sync_config_global_from_runtime(&mut self) {
        self.config.global = GlobalSettingsConfig {
            nm_per_pixel: self.nm_per_pixel,
            sensor_width: self.sensor_width,
            sensor_height: self.sensor_height,
            acq_time_ms: self.acq_time_ms,
            event_store_budget_mib: self.event_store_budget_mib(),
            preview_interval_ms: self.preview_interval_ms,
            point_cloud_interval_ms: self.point_cloud_interval_ms,
            disk_writer_buffer_mib: self.disk_writer_buffer_mib,
        };
    }

    fn published_acq_time_ms(&self) -> u64 {
        if matches!(self.mode, AppMode::Previewing | AppMode::Recording) {
            self.controller
                .as_ref()
                .map(|controller| controller.acq_time_us.load(Ordering::Relaxed) / 1_000)
                .unwrap_or(self.acq_time_ms)
        } else {
            self.acq_time_ms
        }
    }

    fn effective_preview_interval_ms(&self) -> u64 {
        if self.mode == AppMode::Replaying {
            derived_replay_preview_interval_ms(self.acq_time_ms, self.replay_speed)
        } else {
            self.preview_interval_ms
        }
    }

    fn apply_global_config(&mut self, global: &GlobalSettingsConfig) {
        self.nm_per_pixel = global.nm_per_pixel.max(1.0);
        self.sensor_width = global.sensor_width.max(1);
        self.sensor_height = global.sensor_height.max(1);
        self.acq_time_ms = global.acq_time_ms.max(1);
        self.preview_interval_ms = global.preview_interval_ms.max(10);
        self.point_cloud_interval_ms = global.point_cloud_interval_ms.max(20);
        self.disk_writer_buffer_mib = global.disk_writer_buffer_mib.max(1);
        self.event_store
            .set_memory_budget(mib_to_bytes(global.event_store_budget_mib.max(1)));
        self.sync_config_global_from_runtime();
    }

    fn with_active_viewer<R>(&self, f: impl FnOnce(&ViewerState) -> R) -> R {
        if self.popup_open {
            let data = self.popup_shared.lock().unwrap();
            f(&data.viewer)
        } else {
            f(&self.viewer)
        }
    }

    fn with_active_viewer_mut<R>(&mut self, f: impl FnOnce(&mut ViewerState) -> R) -> R {
        if self.popup_open {
            let mut data = self.popup_shared.lock().unwrap();
            f(&mut data.viewer)
        } else {
            f(&mut self.viewer)
        }
    }

    fn active_view_mode(&self) -> ViewMode {
        self.with_active_viewer(|viewer| viewer.view_mode)
    }

    fn open_popup_viewer(&mut self) {
        if self.popup_open {
            return;
        }
        let mut data = self.popup_shared.lock().unwrap();
        data.viewer = std::mem::take(&mut self.viewer);
        data.close_requested = false;
        data.output = None;
        self.popup_open = true;
    }

    fn close_popup_viewer(&mut self) {
        if !self.popup_open {
            return;
        }
        let mut data = self.popup_shared.lock().unwrap();
        self.viewer = std::mem::take(&mut data.viewer);
        data.close_requested = false;
        data.output = None;
        self.popup_open = false;
    }

    fn viewer_replay_state(&self) -> ViewerReplayState {
        let (duration_us, data_len) = self
            .replay_file_info
            .as_ref()
            .map(|info| (info.total_duration_us, info.data_len()))
            .unwrap_or((0, 0));
        ViewerReplayState {
            active: self.mode == AppMode::Replaying && duration_us > 0,
            paused: self.replay_paused,
            finished: self.replay_finished,
            speed: self.replay_speed,
            fraction: self.current_replay_fraction(),
            duration_us,
            time_us: self.current_replay_time_us(),
            bytes_read: self.current_replay_bytes_read(),
            data_len,
        }
    }

    fn current_external_streaming_label(&self) -> String {
        format!(
            "Streaming to ImageJ ({}:{})",
            self.imagej_dialog.host, self.imagej_dialog.port
        )
    }

    fn sync_popup_shared(
        &mut self,
        settings_locked: bool,
        external_streaming: bool,
        external_streaming_label: &str,
    ) {
        if !self.popup_open {
            return;
        }

        let pipeline_stats = self
            .controller
            .as_ref()
            .map(PipelineController::stats_snapshot);
        let detected_hotpixels = self.latest_detected_hotpixels();
        let mut data = self.popup_shared.lock().unwrap();
        data.texture = self.texture.clone();
        data.frame = self.latest_frame.clone();
        data.overlays = self.analysis_output.overlays.clone();
        data.camera_info = self.camera_info.clone();
        data.nm_per_pixel = self.nm_per_pixel;
        data.config = self.config.clone();
        data.mode = self.mode;
        data.settings_locked = settings_locked;
        data.pipeline_stats = pipeline_stats;
        data.replay = self.viewer_replay_state();
        data.analysis_warnings = self.analysis_output.warnings.clone();
        data.analysis_notice = self.analysis_notice.clone();
        data.detected_hotpixels = detected_hotpixels;
        data.config_dirty = self.config_dirty;
        data.acq_dirty = self.acq_dirty;
        data.replay_open_task_active = self.replay_open_task.is_some();
        data.replay_notice = self.replay_notice.clone();
        data.last_error = self.last_error.clone();
        data.external_streaming = external_streaming;
        data.external_streaming_label = external_streaming_label.to_owned();
        data.preview_interval_ms = self.effective_preview_interval_ms();
        data.point_cloud_interval_ms = self.point_cloud_interval_ms;
    }

    fn handle_viewer_output(
        &mut self,
        ctx: &egui::Context,
        output: ViewerOutput,
        from_popup: bool,
    ) {
        if output.popup_toggled {
            if from_popup || self.popup_open {
                self.close_popup_viewer();
            } else {
                self.open_popup_viewer();
            }
        }

        if let Some(new_roi) = output.new_roi {
            self.config.roi = new_roi;
            if self.mode != AppMode::Replaying {
                self.config_dirty = true;
            }
        }

        if output.return_from_external {
            self.disconnect_external_tool();
        }
        if output.mask_hotpixels_clicked {
            self.copy_detected_hotpixels_to_mask();
        }
        if output.replay_toggle_pause {
            self.set_replay_paused(!self.replay_paused);
        }
        if output.replay_restart {
            self.restart_replay();
        }
        if output.replay_stop {
            self.stop_pipeline();
        }
        if let Some(fraction) = output.replay_seek_to {
            self.seek_replay(fraction);
        }
        if let Some(speed) = output.replay_set_speed {
            self.set_replay_speed(speed);
        }

        if output.needs_preview_refresh() {
            self.refresh_preview_if_needed(ctx, true);
        }
    }

    fn external_tool_status(&self) -> ExternalToolStatus {
        self.external_tool
            .as_ref()
            .map(|tool| tool.status())
            .unwrap_or(ExternalToolStatus::Disconnected)
    }

    fn disconnect_external_tool(&mut self) {
        if let Some(mut tool) = self.external_tool.take() {
            tool.disconnect();
        }
    }

    fn show_imagej_dialog(&mut self, ctx: &egui::Context) {
        if !self.imagej_dialog.open {
            return;
        }

        let mut open = self.imagej_dialog.open;
        let mut connect_requested = false;
        let mut disconnect_requested = false;
        let mut export_plugin_requested = false;
        let external_status = self.external_tool_status();
        egui::Window::new("Stream to ImageJ")
            .open(&mut open)
            .collapsible(false)
            .default_width(380.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Scale font sizes based on the available width so the dialog
                // becomes more readable when the user drags the resize handle.
                let base_width = 380.0_f32;
                let scale = (ui.available_width() / base_width).clamp(1.0, 2.0);
                let body_size = 14.0 * scale;
                let small_size = 12.0 * scale;

                let (status_label, status_color) = match &external_status {
                    ExternalToolStatus::Disconnected => (
                        "Disconnected",
                        ui.visuals().widgets.inactive.fg_stroke.color,
                    ),
                    ExternalToolStatus::Connecting => ("Connecting...", analysis_info_color()),
                    ExternalToolStatus::Streaming => ("Connected", status_success_color()),
                    ExternalToolStatus::Error(_) => ("Error", ui.visuals().error_fg_color),
                };

                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Status:").size(body_size));
                    ui.label(
                        egui::RichText::new(status_label)
                            .size(body_size)
                            .color(status_color),
                    );
                });
                ui.add_space(4.0 * scale);

                ui.separator();

                // --- Setup instructions ---
                ui.add_space(2.0 * scale);
                ui.label(egui::RichText::new("Plugin Setup").size(body_size).strong());
                ui.add_space(2.0 * scale);

                let steps = [
                    "Export the plugin with the button below.",
                    "Install it in ImageJ/Fiji via Plugins > Install...",
                    "Or drag it onto ImageJ/Fiji or copy it into the main plugins/ folder.",
                    "Do not place it in plugins/Tools (reserved for toolbar tools).",
                    "Restart ImageJ/Fiji (or Help > Refresh Menus).",
                    "Start the bridge: Plugins > Augur > Start Bridge.",
                ];
                for (i, step) in steps.iter().enumerate() {
                    ui.label(egui::RichText::new(format!("{}. {step}", i + 1)).size(small_size));
                }

                ui.add_space(6.0 * scale);
                if ui
                    .add(egui::Button::new(
                        egui::RichText::new(format!("Save {BUNDLED_IMAGEJ_PLUGIN_JAR_NAME}..."))
                            .size(body_size),
                    ))
                    .clicked()
                {
                    export_plugin_requested = true;
                }
                ui.add_space(2.0 * scale);

                ui.separator();

                // --- Connection settings ---
                ui.add_space(2.0 * scale);
                ui.label(egui::RichText::new("Connection").size(body_size).strong());
                ui.add_space(2.0 * scale);

                egui::Grid::new("imagej_conn_grid")
                    .num_columns(2)
                    .spacing([8.0 * scale, 4.0 * scale])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Host").size(small_size));
                        ui.add_sized(
                            [ui.available_width(), 0.0],
                            egui::TextEdit::singleline(&mut self.imagej_dialog.host)
                                .font(egui::TextStyle::Body),
                        );
                        ui.end_row();

                        ui.label(egui::RichText::new("Port").size(small_size));
                        ui.add(
                            egui::DragValue::new(&mut self.imagej_dialog.port)
                                .clamp_range(1..=u16::MAX),
                        );
                        ui.end_row();
                    });

                ui.add_space(4.0 * scale);

                // --- Feedback messages ---
                if let Some(error) =
                    self.imagej_dialog
                        .error
                        .as_deref()
                        .or(match &external_status {
                            ExternalToolStatus::Error(error) => Some(error.as_str()),
                            _ => None,
                        })
                {
                    ui.label(
                        egui::RichText::new(error)
                            .size(small_size)
                            .color(ui.visuals().error_fg_color),
                    );
                }
                if let Some(info) = self.imagej_dialog.info.as_deref() {
                    ui.label(
                        egui::RichText::new(info)
                            .size(small_size)
                            .color(analysis_info_color()),
                    );
                }

                // --- Action buttons ---
                ui.add_space(4.0 * scale);
                ui.horizontal(|ui| {
                    let connect_enabled = self.external_tool.is_none();
                    if ui
                        .add_enabled(
                            connect_enabled,
                            egui::Button::new(egui::RichText::new("Connect").size(body_size)),
                        )
                        .clicked()
                    {
                        connect_requested = true;
                    }
                    if ui
                        .add_enabled(
                            self.external_tool.is_some(),
                            egui::Button::new(egui::RichText::new("Disconnect").size(body_size)),
                        )
                        .clicked()
                    {
                        disconnect_requested = true;
                    }
                });
            });

        if disconnect_requested {
            self.disconnect_external_tool();
            self.imagej_dialog.error = None;
            self.imagej_dialog.info = None;
        }

        if export_plugin_requested {
            self.imagej_dialog.error = None;
            self.imagej_dialog.info = None;
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(BUNDLED_IMAGEJ_PLUGIN_JAR_NAME)
                .save_file()
            {
                match fs::write(&path, BUNDLED_IMAGEJ_PLUGIN_JAR) {
                    Ok(()) => {
                        self.imagej_dialog.info = Some(format!(
                            "Saved {} to {}",
                            BUNDLED_IMAGEJ_PLUGIN_JAR_NAME,
                            path.display()
                        ));
                    }
                    Err(err) => {
                        self.imagej_dialog.error = Some(format!(
                            "failed to save {}: {err}",
                            BUNDLED_IMAGEJ_PLUGIN_JAR_NAME
                        ));
                    }
                }
            }
        }

        if connect_requested {
            self.imagej_dialog.error = None;
            self.imagej_dialog.info = None;
            if self.external_tool.is_some() {
                self.disconnect_external_tool();
            }

            let mut bridge =
                ImageJBridge::new(self.imagej_dialog.host.clone(), self.imagej_dialog.port);
            match bridge.connect() {
                Ok(()) => {
                    self.external_tool = Some(Box::new(bridge));
                }
                Err(err) => {
                    self.imagej_dialog.error = Some(err);
                }
            }
        }

        self.imagej_dialog.open = open;
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
        self.persistent_context_data.clear();
        self.event_store.clear();
        self.analysis_output = AnalysisOutput::default();
        self.analysis_notice = None;
        self.mark_host_view_datasets_stale();
        self.refresh_host_view_registry();
    }

    fn mark_host_view_datasets_stale(&mut self) {
        self.host_view_dataset_cache.clear();
        for state in self.host_view_render_state.values_mut() {
            state.mark_dirty();
        }
    }

    fn refresh_host_view_registry_if_dirty(&mut self) {
        if !self.host_view_registry_dirty {
            return;
        }
        self.refresh_host_view_registry();
    }

    fn refresh_host_view_registry(&mut self) {
        self.host_view_registry_dirty = false;
        let mut contributions = Vec::new();
        let mut warnings = Vec::new();

        for (index, plugin) in self.builtin_plugins.iter().enumerate() {
            if !plugin.enabled() {
                continue;
            }

            contributions.push(HostRegistryContribution {
                provider: HostViewProviderKey::Builtin(index),
                provider_name: plugin.name().to_owned(),
                registry: plugin.host_views(),
            });
        }

        for (index, record) in self.plugin_manager.records().iter().enumerate() {
            let Some(plugin) = record.plugin() else {
                continue;
            };
            if !plugin.enabled() {
                continue;
            }

            match plugin.host_views() {
                Ok(registry) => contributions.push(HostRegistryContribution {
                    provider: HostViewProviderKey::Runtime(index),
                    provider_name: plugin.name().to_owned(),
                    registry,
                }),
                Err(err) => warnings.push(format!(
                    "Failed to read host views from {}: {err}",
                    plugin.name()
                )),
            }
        }

        let resolved = resolve_host_view_registry(contributions);
        warnings.extend(resolved.warnings().iter().cloned());
        if warnings != self.host_view_resolution_warnings {
            for warning in &warnings {
                eprintln!("host view registry warning: {warning}");
            }
            self.host_view_resolution_warnings = warnings;
        }

        self.host_view_window_open
            .retain(|view_id, _| resolved.view(view_id).is_some());
        self.host_view_render_state
            .retain(|view_id, _| resolved.view(view_id).is_some());
        self.host_view_dataset_cache
            .retain(|dataset_id, _| resolved.dataset(dataset_id).is_some());
        self.host_view_registry = resolved;
    }

    fn load_host_view_dataset_snapshot(&self, dataset_id: &str) -> CachedHostDataset {
        let Some(dataset) = self.host_view_registry.dataset(dataset_id) else {
            return Err(format!("unknown host dataset {dataset_id}"));
        };

        let bytes = match dataset.provider {
            HostViewProviderKey::Builtin(index) => self
                .builtin_plugins
                .get(index)
                .and_then(|plugin| {
                    plugin
                        .enabled()
                        .then(|| plugin.host_view_dataset(dataset_id))
                })
                .flatten(),
            HostViewProviderKey::Runtime(index) => {
                let Some(record) = self.plugin_manager.records().get(index) else {
                    return Err(format!(
                        "runtime provider index {index} is out of range for dataset {dataset_id}"
                    ));
                };
                let Some(plugin) = record.plugin() else {
                    return Err(format!(
                        "runtime provider {} is unavailable for dataset {dataset_id}",
                        record.name()
                    ));
                };
                plugin.host_view_dataset(dataset_id)?
            }
        };

        match bytes {
            Some(bytes) => decode_dataset_snapshot(&dataset.descriptor, &bytes).map(Some),
            None => Ok(None),
        }
    }

    fn current_host_view_dataset_generation(&self, dataset_id: &str) -> Result<u64, String> {
        let Some(dataset) = self.host_view_registry.dataset(dataset_id) else {
            return Err(format!("unknown host dataset {dataset_id}"));
        };

        Ok(match dataset.provider {
            HostViewProviderKey::Builtin(index) => self
                .builtin_plugins
                .get(index)
                .and_then(|plugin| {
                    plugin
                        .enabled()
                        .then(|| plugin.host_view_dataset_generation(dataset_id))
                })
                .unwrap_or(0),
            HostViewProviderKey::Runtime(index) => {
                let Some(record) = self.plugin_manager.records().get(index) else {
                    return Err(format!(
                        "runtime provider index {index} is out of range for dataset {dataset_id}"
                    ));
                };
                let Some(plugin) = record.plugin() else {
                    return Err(format!(
                        "runtime provider {} is unavailable for dataset {dataset_id}",
                        record.name()
                    ));
                };
                plugin.host_view_dataset_generation(dataset_id)
            }
        })
    }

    fn ensure_host_view_dataset_cached(&mut self, dataset_id: &str) {
        let generation = match self.current_host_view_dataset_generation(dataset_id) {
            Ok(generation) => generation,
            Err(err) => {
                self.host_view_dataset_cache.insert(
                    dataset_id.to_owned(),
                    HostDatasetCacheEntry {
                        generation: 0,
                        snapshot: Err(err),
                    },
                );
                return;
            }
        };

        if self
            .host_view_dataset_cache
            .get(dataset_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            return;
        }
        self.host_view_dataset_cache.insert(
            dataset_id.to_owned(),
            HostDatasetCacheEntry {
                generation,
                snapshot: self.load_host_view_dataset_snapshot(dataset_id),
            },
        );
    }

    fn export_host_view_csv(&mut self, view: &ResolvedHostView) {
        let Some(dataset) = self
            .host_view_registry
            .dataset(&view.descriptor.dataset_id)
            .cloned()
        else {
            self.last_error = Some(format!(
                "dataset {} is no longer available",
                view.descriptor.dataset_id
            ));
            return;
        };
        let HostDatasetKind::TableV1(schema) = &dataset.descriptor.kind else {
            self.last_error = Some(format!(
                "{} is not a tabular dataset and cannot be exported as CSV",
                dataset.descriptor.title
            ));
            return;
        };

        self.ensure_host_view_dataset_cached(&view.descriptor.dataset_id);
        let table = match cached_table(
            self.host_view_dataset_cache
                .get(&view.descriptor.dataset_id),
        ) {
            Ok(Some(table)) => table,
            Ok(None) => {
                self.last_error = Some("no dataset is available to export".into());
                return;
            }
            Err(err) => {
                self.last_error = Some(err.to_owned());
                return;
            }
        };

        let default_name = format!("{}.csv", sanitize_file_stem(&view.descriptor.title));
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .add_filter("CSV", &["csv"])
            .save_file()
        else {
            return;
        };

        match export_table_csv_to_path(&path, schema, table) {
            Ok(()) => self.last_error = None,
            Err(err) => self.last_error = Some(err),
        }
    }

    fn export_host_view_image(&mut self, view: &ResolvedHostView, format: HostViewImageFormat) {
        let image = match self.host_view_render_state.get(&view.descriptor.id) {
            Some(HostViewRenderState::Density2d(state)) => state.image(),
            Some(HostViewRenderState::Image2d(state)) => state.image(),
            _ => None,
        };
        let Some(image) = image else {
            self.last_error = Some("no host view image is available to export".into());
            return;
        };

        let default_name = format!(
            "{}.{}",
            sanitize_file_stem(&view.descriptor.title),
            format.extension()
        );
        let Some(path) = rfd::FileDialog::new()
            .set_file_name(&default_name)
            .add_filter("PNG", &["png"])
            .add_filter("TIFF", &["tif", "tiff"])
            .save_file()
        else {
            return;
        };

        match export_image_to_path(&path, image) {
            Ok(()) => self.last_error = None,
            Err(err) => self.last_error = Some(err),
        }
    }

    fn clear_host_view_provider(&mut self, dataset_id: &str) {
        let Some(dataset) = self.host_view_registry.dataset(dataset_id).cloned() else {
            return;
        };

        reset_provider_for_dataset(
            &dataset,
            |index| {
                if let Some(plugin) = self.builtin_plugins.get_mut(index) {
                    plugin.reset();
                }
            },
            |index| {
                if let Some(plugin) = self
                    .plugin_manager
                    .records_mut()
                    .get_mut(index)
                    .and_then(|record| record.plugin_mut())
                {
                    plugin.reset();
                }
            },
        );

        self.mark_host_view_datasets_stale();
        self.refresh_host_view_registry();
    }

    fn handle_host_view_actions(&mut self, view: &ResolvedHostView, actions: HostViewUiActions) {
        if actions.clear_requested {
            self.clear_host_view_provider(&view.descriptor.dataset_id);
        }
        if actions.export_csv {
            self.export_host_view_csv(view);
        }
        if let Some(format) = actions.export_image {
            self.export_host_view_image(view, format);
        }
    }

    fn render_host_view_content(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        view: &ResolvedHostView,
    ) {
        let Some(dataset) = self
            .host_view_registry
            .dataset(&view.descriptor.dataset_id)
            .cloned()
        else {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Dataset {} is unavailable.", view.descriptor.dataset_id),
            );
            return;
        };

        self.ensure_host_view_dataset_cached(&view.descriptor.dataset_id);
        let cache_entry = self
            .host_view_dataset_cache
            .get(&view.descriptor.dataset_id);
        let dataset_generation = cache_entry.map(|entry| entry.generation).unwrap_or(0);

        match (&view.descriptor.kind, &dataset.descriptor.kind) {
            (HostViewKind::CompactTable, HostDatasetKind::TableV1(schema)) => {
                match cached_table(cache_entry) {
                    Ok(table) => {
                        render_compact_table(ui, schema, table, &dataset.descriptor.empty_message);
                    }
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                }
            }
            (HostViewKind::TableWindow, HostDatasetKind::TableV1(schema)) => {
                let actions = match cached_table(cache_entry) {
                    Ok(table) => {
                        render_table_window(ui, schema, table, &dataset.descriptor.empty_message)
                    }
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        HostViewUiActions::default()
                    }
                };
                self.handle_host_view_actions(view, actions);
            }
            (
                HostViewKind::Density2dFromTable { x_column, y_column },
                HostDatasetKind::TableV1(schema),
            ) => {
                let actions = {
                    let state = self
                        .host_view_render_state
                        .entry(view.descriptor.id.clone())
                        .or_default()
                        .density_state();
                    let table = match cached_table(cache_entry) {
                        Ok(table) => table,
                        Err(err) => {
                            ui.colored_label(ui.visuals().error_fg_color, err);
                            return;
                        }
                    };

                    if let Err(err) = state.render_if_needed(
                        ctx,
                        schema,
                        table,
                        dataset_generation,
                        x_column,
                        y_column,
                    ) {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        return;
                    }

                    render_density2d_view(
                        ui,
                        &view.descriptor.id,
                        state,
                        table,
                        &dataset.descriptor.empty_message,
                        true,
                    )
                };
                self.handle_host_view_actions(view, actions);
            }
            (
                HostViewKind::Scatter2dFromTable { x_column, y_column },
                HostDatasetKind::TableV1(schema),
            ) => {
                let actions = match cached_table(cache_entry) {
                    Ok(table) => render_scatter2d_view(
                        ui,
                        schema,
                        table,
                        &dataset.descriptor.empty_message,
                        Scatter2dViewOptions {
                            view_id: &view.descriptor.id,
                            x_column,
                            y_column,
                            allow_clear: true,
                        },
                    ),
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        HostViewUiActions::default()
                    }
                };
                self.handle_host_view_actions(view, actions);
            }
            (HostViewKind::ImageWindow, HostDatasetKind::Image2dV1) => {
                let actions = {
                    let state = self
                        .host_view_render_state
                        .entry(view.descriptor.id.clone())
                        .or_default()
                        .image_state();
                    let image = match cached_image(cache_entry) {
                        Ok(image) => image,
                        Err(err) => {
                            ui.colored_label(ui.visuals().error_fg_color, err);
                            return;
                        }
                    };
                    if let Err(err) = state.render_if_needed(ctx, image, dataset_generation) {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        return;
                    }
                    render_image2d_view(
                        ui,
                        &view.descriptor.id,
                        state,
                        image,
                        &dataset.descriptor.empty_message,
                        true,
                    )
                };
                self.handle_host_view_actions(view, actions);
            }
            (HostViewKind::LineSeriesWindow, HostDatasetKind::Series1dV1) => {
                match cached_series(cache_entry) {
                    Ok(series) => {
                        render_line_series_view(
                            ui,
                            &view.descriptor.id,
                            series,
                            &dataset.descriptor.empty_message,
                        );
                    }
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                }
            }
            _ => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!(
                        "View {} does not match dataset {}.",
                        view.descriptor.id, dataset.descriptor.id
                    ),
                );
            }
        }
    }

    fn render_provider_host_views(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        provider: HostViewProviderKey,
    ) {
        let views: Vec<ResolvedHostView> = self
            .host_view_registry
            .panel_views_for_provider(provider)
            .cloned()
            .collect();
        if views.is_empty() {
            return;
        }

        egui::CollapsingHeader::new("Host Views")
            .default_open(false)
            .show(ui, |ui| {
                for (index, view) in views.iter().enumerate() {
                    ui.label(egui::RichText::new(&view.descriptor.title).strong());
                    self.render_host_view_content(ctx, ui, view);
                    if index + 1 < views.len() {
                        ui.separator();
                    }
                }
            });
    }

    fn render_host_view_windows(&mut self, ctx: &egui::Context) {
        let views: Vec<ResolvedHostView> =
            self.host_view_registry.window_views().cloned().collect();
        for view in views {
            if !self
                .host_view_window_open
                .get(&view.descriptor.id)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }

            let Some(dataset) = self
                .host_view_registry
                .dataset(&view.descriptor.dataset_id)
                .cloned()
            else {
                self.host_view_window_open
                    .insert(view.descriptor.id.clone(), false);
                continue;
            };

            self.ensure_host_view_dataset_cached(&view.descriptor.dataset_id);
            let cache_entry = self
                .host_view_dataset_cache
                .get(&view.descriptor.dataset_id)
                .cloned();
            let viewport_id =
                egui::ViewportId::from_hash_of(("host_view_window", &view.descriptor.id));
            let title = format!("{} — AugurRS", view.descriptor.title);

            match (&view.descriptor.kind, &dataset.descriptor.kind) {
                (HostViewKind::TableWindow, HostDatasetKind::TableV1(schema)) => {
                    let (table_arc, error_message) = match &cache_entry {
                        Some(HostDatasetCacheEntry {
                            snapshot: Ok(Some(HostDatasetSnapshot::Table(table))),
                            ..
                        }) => (Some(Arc::clone(table)), None),
                        Some(HostDatasetCacheEntry {
                            snapshot: Err(err), ..
                        }) => (None, Some(err.clone())),
                        _ => (None, None),
                    };
                    let shared = Arc::new(Mutex::new(TableWindowViewportData {
                        schema: schema.clone(),
                        dataset: table_arc,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_csv_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let window_title = title.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| match class {
                            egui::viewport::ViewportClass::Deferred => {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    render_table_window_viewport(ui, &shared_for_viewport);
                                });
                                if ctx.input(|i| i.viewport().close_requested()) {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            egui::viewport::ViewportClass::Embedded => {
                                let mut open = true;
                                egui::Window::new(&window_title)
                                    .open(&mut open)
                                    .default_size([1100.0, 760.0])
                                    .show(ctx, |ui| {
                                        render_table_window_viewport(ui, &shared_for_viewport);
                                    });
                                if !open {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            _ => {}
                        },
                    );

                    let (close, export_csv) = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let result = (data.close_requested, data.export_csv_requested);
                        data.close_requested = false;
                        data.export_csv_requested = false;
                        result
                    };
                    if close {
                        self.host_view_window_open
                            .insert(view.descriptor.id.clone(), false);
                    }
                    if export_csv {
                        self.export_host_view_csv(&view);
                    }
                }
                (
                    HostViewKind::Density2dFromTable { x_column, y_column },
                    HostDatasetKind::TableV1(schema),
                ) => {
                    let (settings, texture, rendered_size, total_rows, error_message) = {
                        let state = self
                            .host_view_render_state
                            .entry(view.descriptor.id.clone())
                            .or_default()
                            .density_state();
                        let dataset_generation = cache_entry
                            .as_ref()
                            .map(|entry| entry.generation)
                            .unwrap_or(0);

                        let table_result = cached_table(cache_entry.as_ref());
                        let table = table_result.as_ref().ok().and_then(|table| *table);
                        let render_result = match table_result {
                            Ok(table) => state.render_if_needed(
                                ctx,
                                schema,
                                table,
                                dataset_generation,
                                x_column,
                                y_column,
                            ),
                            Err(err) => Err(err.to_owned()),
                        };
                        let error_message = match (&cache_entry, render_result) {
                            (
                                Some(HostDatasetCacheEntry {
                                    snapshot: Err(err), ..
                                }),
                                _,
                            ) => Some(err.clone()),
                            (_, Err(err)) => Some(err),
                            _ => None,
                        };

                        let total_rows = table.map(|table| table.row_count()).unwrap_or(0);
                        (
                            state.settings(),
                            state.texture().cloned(),
                            state.rendered_size(),
                            total_rows,
                            error_message,
                        )
                    };

                    let shared = Arc::new(Mutex::new(DensityWindowViewportData {
                        texture,
                        total_rows,
                        rendered_width: rendered_size[0],
                        rendered_height: rendered_size[1],
                        settings,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_csv_requested: false,
                        export_image_requested: None,
                        clear_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let view_id = view.descriptor.id.clone();
                    let window_title = title.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1200.0, 860.0]),
                        move |ctx, class| match class {
                            egui::viewport::ViewportClass::Deferred => {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    render_density_window_viewport(
                                        ui,
                                        &view_id,
                                        &shared_for_viewport,
                                    );
                                });
                                if ctx.input(|i| i.viewport().close_requested()) {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            egui::viewport::ViewportClass::Embedded => {
                                let mut open = true;
                                egui::Window::new(&window_title)
                                    .open(&mut open)
                                    .default_size([1100.0, 760.0])
                                    .show(ctx, |ui| {
                                        render_density_window_viewport(
                                            ui,
                                            &view_id,
                                            &shared_for_viewport,
                                        );
                                    });
                                if !open {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            _ => {}
                        },
                    );

                    let (next_settings, close, clear, export_csv, export_image) = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let result = (
                            data.settings,
                            data.close_requested,
                            data.clear_requested,
                            data.export_csv_requested,
                            data.export_image_requested.take(),
                        );
                        data.close_requested = false;
                        data.export_csv_requested = false;
                        data.clear_requested = false;
                        result
                    };

                    self.host_view_render_state
                        .entry(view.descriptor.id.clone())
                        .or_default()
                        .density_state()
                        .set_settings(next_settings);

                    if close {
                        self.host_view_window_open
                            .insert(view.descriptor.id.clone(), false);
                    }
                    if clear {
                        self.clear_host_view_provider(&view.descriptor.dataset_id);
                    }
                    if export_csv {
                        self.export_host_view_csv(&view);
                    }
                    if let Some(format) = export_image {
                        self.export_host_view_image(&view, format);
                    }
                }
                (
                    HostViewKind::Scatter2dFromTable { x_column, y_column },
                    HostDatasetKind::TableV1(schema),
                ) => {
                    let (table_arc, error_message) = match &cache_entry {
                        Some(HostDatasetCacheEntry {
                            snapshot: Ok(Some(HostDatasetSnapshot::Table(table))),
                            ..
                        }) => (Some(Arc::clone(table)), None),
                        Some(HostDatasetCacheEntry {
                            snapshot: Err(err), ..
                        }) => (None, Some(err.clone())),
                        _ => (None, None),
                    };
                    let shared = Arc::new(Mutex::new(ScatterWindowViewportData {
                        schema: schema.clone(),
                        dataset: table_arc,
                        x_column: x_column.clone(),
                        y_column: y_column.clone(),
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_csv_requested: false,
                        clear_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let view_id = view.descriptor.id.clone();
                    let window_title = title.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| match class {
                            egui::viewport::ViewportClass::Deferred => {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    render_scatter_window_viewport(
                                        ui,
                                        &view_id,
                                        &shared_for_viewport,
                                    );
                                });
                                if ctx.input(|i| i.viewport().close_requested()) {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            egui::viewport::ViewportClass::Embedded => {
                                let mut open = true;
                                egui::Window::new(&window_title)
                                    .open(&mut open)
                                    .default_size([1100.0, 760.0])
                                    .show(ctx, |ui| {
                                        render_scatter_window_viewport(
                                            ui,
                                            &view_id,
                                            &shared_for_viewport,
                                        );
                                    });
                                if !open {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            _ => {}
                        },
                    );

                    let (close, clear, export_csv) = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let result = (
                            data.close_requested,
                            data.clear_requested,
                            data.export_csv_requested,
                        );
                        data.close_requested = false;
                        data.clear_requested = false;
                        data.export_csv_requested = false;
                        result
                    };
                    if close {
                        self.host_view_window_open
                            .insert(view.descriptor.id.clone(), false);
                    }
                    if clear {
                        self.clear_host_view_provider(&view.descriptor.dataset_id);
                    }
                    if export_csv {
                        self.export_host_view_csv(&view);
                    }
                }
                (HostViewKind::ImageWindow, HostDatasetKind::Image2dV1) => {
                    let (settings, texture, rendered_size, error_message) = {
                        let state = self
                            .host_view_render_state
                            .entry(view.descriptor.id.clone())
                            .or_default()
                            .image_state();
                        let dataset_generation = cache_entry
                            .as_ref()
                            .map(|entry| entry.generation)
                            .unwrap_or(0);
                        let image_result = cached_image(cache_entry.as_ref());
                        let render_result = match image_result {
                            Ok(image) => state.render_if_needed(ctx, image, dataset_generation),
                            Err(err) => Err(err.to_owned()),
                        };
                        let error_message = match (&cache_entry, render_result) {
                            (
                                Some(HostDatasetCacheEntry {
                                    snapshot: Err(err), ..
                                }),
                                _,
                            ) => Some(err.clone()),
                            (_, Err(err)) => Some(err),
                            _ => None,
                        };
                        (
                            state.settings(),
                            state.texture().cloned(),
                            state.rendered_size(),
                            error_message,
                        )
                    };

                    let shared = Arc::new(Mutex::new(ImageWindowViewportData {
                        texture,
                        rendered_width: rendered_size[0],
                        rendered_height: rendered_size[1],
                        settings,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_image_requested: None,
                        clear_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let view_id = view.descriptor.id.clone();
                    let window_title = title.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| match class {
                            egui::viewport::ViewportClass::Deferred => {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    render_image_window_viewport(
                                        ui,
                                        &view_id,
                                        &shared_for_viewport,
                                    );
                                });
                                if ctx.input(|i| i.viewport().close_requested()) {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            egui::viewport::ViewportClass::Embedded => {
                                let mut open = true;
                                egui::Window::new(&window_title)
                                    .open(&mut open)
                                    .default_size([1100.0, 760.0])
                                    .show(ctx, |ui| {
                                        render_image_window_viewport(
                                            ui,
                                            &view_id,
                                            &shared_for_viewport,
                                        );
                                    });
                                if !open {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            _ => {}
                        },
                    );

                    let (next_settings, close, clear, export_image) = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let result = (
                            data.settings,
                            data.close_requested,
                            data.clear_requested,
                            data.export_image_requested.take(),
                        );
                        data.close_requested = false;
                        data.clear_requested = false;
                        result
                    };

                    self.host_view_render_state
                        .entry(view.descriptor.id.clone())
                        .or_default()
                        .image_state()
                        .set_settings(next_settings);

                    if close {
                        self.host_view_window_open
                            .insert(view.descriptor.id.clone(), false);
                    }
                    if clear {
                        self.clear_host_view_provider(&view.descriptor.dataset_id);
                    }
                    if let Some(format) = export_image {
                        self.export_host_view_image(&view, format);
                    }
                }
                (HostViewKind::LineSeriesWindow, HostDatasetKind::Series1dV1) => {
                    let (series_arc, error_message) = match &cache_entry {
                        Some(HostDatasetCacheEntry {
                            snapshot: Ok(Some(HostDatasetSnapshot::Series1d(series))),
                            ..
                        }) => (Some(Arc::clone(series)), None),
                        Some(HostDatasetCacheEntry {
                            snapshot: Err(err), ..
                        }) => (None, Some(err.clone())),
                        _ => (None, None),
                    };
                    let shared = Arc::new(Mutex::new(SeriesWindowViewportData {
                        dataset: series_arc,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let view_id = view.descriptor.id.clone();
                    let window_title = title.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| match class {
                            egui::viewport::ViewportClass::Deferred => {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    render_series_window_viewport(
                                        ui,
                                        &view_id,
                                        &shared_for_viewport,
                                    );
                                });
                                if ctx.input(|i| i.viewport().close_requested()) {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            egui::viewport::ViewportClass::Embedded => {
                                let mut open = true;
                                egui::Window::new(&window_title)
                                    .open(&mut open)
                                    .default_size([1100.0, 760.0])
                                    .show(ctx, |ui| {
                                        render_series_window_viewport(
                                            ui,
                                            &view_id,
                                            &shared_for_viewport,
                                        );
                                    });
                                if !open {
                                    if let Ok(mut data) = shared_for_viewport.lock() {
                                        data.close_requested = true;
                                    }
                                }
                            }
                            _ => {}
                        },
                    );

                    let close = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let close = data.close_requested;
                        data.close_requested = false;
                        close
                    };
                    if close {
                        self.host_view_window_open
                            .insert(view.descriptor.id.clone(), false);
                    }
                }
                _ => {}
            }
        }
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

    fn plugins_need_retained_event_history(&self) -> bool {
        self.builtin_plugins
            .iter()
            .any(|plugin| plugin.enabled() && plugin.capabilities().retained_event_history)
            || self.plugin_manager.records().iter().any(|record| {
                record.plugin().is_some_and(|plugin| {
                    plugin.enabled() && plugin.capabilities().retained_event_history
                })
            })
    }

    fn raw_events_required(&self) -> bool {
        self.active_view_mode() == ViewMode::PointCloud3d
            || self.with_active_viewer(|viewer| viewer.preview_mode.requires_raw_events())
            || self.plugins_need_raw_events()
            || self.plugins_need_retained_event_history()
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
        if self.mode != AppMode::Idle || self.replay_open_task.is_some() {
            return;
        }
        match self.start_pipeline_inner(true) {
            Ok(controller) => {
                self.sync_pipeline_requirements(&controller);
                self.controller = Some(controller);
                self.last_preview_process_at = None;
                self.mode = AppMode::Previewing;
                self.with_active_viewer_mut(ViewerState::clear_session_state);
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
        if self.mode == AppMode::Recording || self.replay_open_task.is_some() {
            return;
        }
        if self.mode == AppMode::Previewing {
            self.stop_pipeline();
        }
        match self.start_pipeline_inner(false) {
            Ok(controller) => {
                self.sync_pipeline_requirements(&controller);
                self.controller = Some(controller);
                self.last_preview_process_at = None;
                self.mode = AppMode::Recording;
                self.with_active_viewer_mut(ViewerState::clear_session_state);
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
        if self.mode != AppMode::Idle || self.replay_open_task.is_some() {
            return;
        }

        self.sync_config_global_from_runtime();

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

        let extension = replay_file_extension(&path);
        let open_result = match extension.as_deref() {
            Some("raw") => match RawFileCamera::open(&path) {
                Ok((camera, controls, info)) => {
                    let replay_info = camera.device_info();
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.replay_timestamp_feedback =
                        Some(Arc::clone(&controls.current_timestamp_us));
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::default(),
                        replay_pipeline_config(&info),
                        options,
                    )
                    .map(|controller| OpenedReplay {
                        controller,
                        controls,
                        info,
                        replay_info,
                        decoded_events: None,
                    })
                    .map_err(|err| format!("pipeline start failed: {err}"))
                }
                Err(err) => Err(format!("open replay file failed: {err}")),
            },
            Some("csv") | Some("bin") | Some("npy") | Some("h5") | Some("hdf5") => {
                let (tx, rx) = mpsc::channel();
                let path_for_thread = path.clone();
                thread::spawn(move || {
                    let result = match DecodedEventFileCamera::open(&path_for_thread) {
                        Ok((camera, controls, info, decoded_events)) => {
                            let replay_info = camera.device_info();
                            let mut options =
                                PipelineOptions::preview_only(info.width, info.height);
                            options.replay_timestamp_feedback =
                                Some(Arc::clone(&controls.current_timestamp_us));
                            spawn_pipeline(
                                camera,
                                PackedEventPreviewDecoder::default(),
                                replay_pipeline_config(&info),
                                options,
                            )
                            .map(|controller| OpenedReplay {
                                controller,
                                controls,
                                info,
                                replay_info,
                                decoded_events: Some(decoded_events),
                            })
                            .map_err(|err| format!("pipeline start failed: {err}"))
                        }
                        Err(err) => Err(format!("open replay file failed: {err}")),
                    };
                    let _ = tx.send(result);
                });
                self.replay_open_task = Some(ReplayOpenTask {
                    path: path.clone(),
                    saved_live_state,
                    rx,
                });
                self.replay_notice = Some("Opening replay...".into());
                self.last_error = None;
                self.camera_status = format!(
                    "Opening {}…",
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.display().to_string())
                );
                return;
            }
            Some(ext) => Err(format!("unsupported replay file extension: .{ext}")),
            None => Err("replay file is missing an extension".into()),
        };
        let opened = match open_result {
            Ok(result) => result,
            Err(err) => {
                self.last_error = Some(err);
                return;
            }
        };
        self.finish_opened_replay(path, saved_live_state, opened);
    }

    fn finish_opened_replay(
        &mut self,
        path: PathBuf,
        saved_live_state: SavedLiveState,
        opened: OpenedReplay,
    ) {
        let OpenedReplay {
            controller,
            controls,
            info,
            replay_info,
            decoded_events,
        } = opened;
        let (display_config, display_mask_file, replay_notice) =
            self.load_replay_display_settings(&path, &info);

        self.set_replay_paused_internal(&controls, false);
        self.set_replay_speed_internal(&controls, 1.0);
        self.sync_pipeline_requirements(&controller);
        self.controller = Some(controller);
        self.last_preview_process_at = None;
        self.mode = AppMode::Replaying;
        self.with_active_viewer_mut(ViewerState::clear_session_state);
        self.camera_info = Some(replay_info);
        self.config = display_config;
        let global = self.config.global.clone();
        self.apply_global_config(&global);
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

    fn poll_replay_open_task(&mut self) {
        let Some(task) = self.replay_open_task.take() else {
            return;
        };

        match task.rx.try_recv() {
            Ok(Ok(opened)) => {
                self.finish_opened_replay(task.path, task.saved_live_state, opened);
            }
            Ok(Err(err)) => {
                self.replay_notice = None;
                self.last_error = Some(err);
                self.camera_status =
                    "Camera idle. Current local settings will be used for the next recording."
                        .into();
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.replay_open_task = Some(task);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.replay_notice = None;
                self.last_error = Some("replay open worker disconnected unexpectedly".into());
                self.camera_status =
                    "Camera idle. Current local settings will be used for the next recording."
                        .into();
            }
        }
    }

    fn current_export_event_source(&self) -> Result<ExportEventSource, String> {
        if let Some(events) = &self.replay_decoded_events {
            return Ok(ExportEventSource::Decoded(Arc::clone(events)));
        }

        let path = self
            .replay_path
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| "replay path is missing".to_owned())?;
        let info = self
            .replay_file_info
            .as_ref()
            .ok_or_else(|| "replay file metadata is missing".to_owned())?;

        Ok(ExportEventSource::RawEvt3 {
            path,
            data_offset: info.data_offset,
        })
    }

    fn open_tiff_stack_export_dialog(&mut self) {
        let Some(path) = self.replay_path.as_ref().map(PathBuf::from) else {
            self.last_error = Some("replay path is missing".into());
            return;
        };
        let Some(info) = self.replay_file_info.as_ref() else {
            self.last_error = Some("replay file metadata is missing".into());
            return;
        };

        self.export_dialog
            .open_for_replay(&path, info, self.acq_time_ms, self.config.roi);
    }

    fn start_tiff_stack_export(&mut self, mut params: TiffStackExportParams) {
        if self.export_task.is_some() {
            return;
        }

        let source = match self.current_export_event_source() {
            Ok(source) => source,
            Err(err) => {
                self.export_dialog.finish_error(err.clone());
                self.last_error = Some(err);
                return;
            }
        };

        params.output_path = ensure_extension(params.output_path, "tiff");
        let output_path = params.output_path.clone();
        let (tx, rx) = mpsc::channel();
        self.export_dialog.set_exporting(true);
        thread::spawn(move || {
            let result = export_tiff_stack(source, &params);
            let _ = tx.send(result);
        });
        self.export_task = Some(TiffStackExportTask { output_path, rx });
        self.last_error = None;
    }

    fn poll_tiff_stack_export_task(&mut self) {
        let Some(task) = self.export_task.take() else {
            return;
        };

        match task.rx.try_recv() {
            Ok(Ok(frame_count)) => {
                self.export_dialog
                    .finish_success(frame_count, &task.output_path);
                self.last_error = None;
            }
            Ok(Err(err)) => {
                self.export_dialog.finish_error(err.clone());
                self.last_error = Some(err);
            }
            Err(mpsc::TryRecvError::Empty) => {
                self.export_task = Some(task);
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                let message = "TIFF export task ended unexpectedly".to_owned();
                self.export_dialog.finish_error(message.clone());
                self.last_error = Some(message);
            }
        }
    }

    fn load_replay_display_settings(
        &self,
        raw_path: &Path,
        info: &ReplayFileInfo,
    ) -> (CameraConfig, String, Option<String>) {
        let default_config = replay_pipeline_config(info);

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
            Ok(mut config) => {
                if config.global == GlobalSettingsConfig::default() {
                    config.global.sensor_width = info.width;
                    config.global.sensor_height = info.height;
                    if let Some(pixel_pitch_nm) = info.metadata.pixel_pitch_nm {
                        config.global.nm_per_pixel = pixel_pitch_nm;
                    }
                }
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
        self.sync_config_global_from_runtime();

        let options = if preview_only {
            PipelineOptions::preview_only(self.sensor_width, self.sensor_height)
        } else {
            let output_path = self.validated_output_path()?;
            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("failed creating output directory {}: {e}", parent.display())
                })?;
            }
            let mut opts = PipelineOptions::new(output_path);
            opts.sensor_width = self.sensor_width;
            opts.sensor_height = self.sensor_height;
            opts.disk_writer_buffer_bytes = mib_to_bytes(self.disk_writer_buffer_mib);
            opts
        };

        let camera = Evk4Camera::open_imx636().map_err(|e| format!("open camera failed: {e}"))?;
        let camera_info = camera.device_info();
        self.camera_info = Some(camera_info.clone());
        let mut options = options;
        if !preview_only {
            options.metadata = Some(
                RecordingMetadata::from_context(&camera_info, &self.config)
                    .with_annotations(RecordingAnnotations::default()),
            );
        }

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
        let speed_bits = speed.to_bits();
        let previous = controls.speed_bits.swap(speed_bits, Ordering::Relaxed);
        if previous != speed_bits {
            controls.speed_epoch.fetch_add(1, Ordering::Relaxed);
        }
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

    fn preview_display_settings(&self) -> PreviewDisplaySettings {
        self.with_active_viewer(ViewerState::preview_display_settings)
    }

    fn render_preview_image(&self, frame: &PreviewFrame) -> egui::ColorImage {
        let (preview_mode, time_surface_tau_us) =
            self.with_active_viewer(|viewer| (viewer.preview_mode, viewer.time_surface_tau_us));
        frame_to_color_image(
            frame,
            &self.analysis_output.overlays,
            self.preview_display_settings(),
            preview_mode,
            time_surface_tau_us,
        )
    }

    fn refresh_preview_texture_from_latest_frame(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.latest_frame.as_ref() else {
            return;
        };
        let image = self.render_preview_image(frame);
        if let Some(texture) = &mut self.texture {
            texture.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
        }
    }

    fn apply_preview_histogram(&mut self, histogram: Vec<u64>) {
        self.with_active_viewer_mut(|viewer| viewer.apply_histogram(histogram));
    }

    fn refresh_preview_if_needed(&mut self, ctx: &egui::Context, settings_changed: bool) {
        if !settings_changed || self.active_view_mode() != ViewMode::Preview2d {
            return;
        }
        let Some(frame) = self.latest_frame.as_ref() else {
            return;
        };
        let (preview_mode, time_surface_tau_us) =
            self.with_active_viewer(|viewer| (viewer.preview_mode, viewer.time_surface_tau_us));
        let histogram = compute_frame_histogram(frame, preview_mode, time_surface_tau_us);
        self.apply_preview_histogram(histogram);
        if self.mode != AppMode::Idle {
            self.refresh_preview_texture_from_latest_frame(ctx);
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
                Ok((camera, controls)) => {
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.replay_timestamp_feedback =
                        Some(Arc::clone(&controls.current_timestamp_us));
                    spawn_pipeline(
                        camera,
                        PackedEventPreviewDecoder::default(),
                        replay_pipeline_config(&info),
                        options,
                    )
                    .map(|controller| (controller, controls))
                    .map_err(|err| format!("seek pipeline start failed: {err}"))
                }
                Err(err) => Err(format!("seek failed: {err}")),
            }
        } else {
            let target_byte = info.data_offset + align_relative_evt3_word_offset(target_rel);
            match RawFileCamera::open_at(&path, &info, target_byte) {
                Ok((camera, controls)) => {
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.replay_timestamp_feedback =
                        Some(Arc::clone(&controls.current_timestamp_us));
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::default(),
                        replay_pipeline_config(&info),
                        options,
                    )
                    .map(|controller| (controller, controls))
                    .map_err(|err| format!("seek pipeline start failed: {err}"))
                }
                Err(err) => Err(format!("seek failed: {err}")),
            }
        };
        let (controller, controls) = match reopen_result {
            Ok(result) => result,
            Err(err) => {
                self.replay_finished = true;
                self.replay_paused = true;
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
        self.last_preview_process_at = None;
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_paused = desired_paused;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = pause_after_first_frame;
        self.last_error = None;
        self.with_active_viewer_mut(ViewerState::clear_session_state);
        self.camera_status = format!(
            "Replaying {}.",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
        self.latest_frame = None;
        reset_preview_render_cache();
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
            let global = self.config.global.clone();
            self.apply_global_config(&global);
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
        if self.export_task.is_none() {
            self.export_dialog = ExportDialog::default();
        }
        self.restore_saved_live_state();
    }

    fn stop_pipeline(&mut self) {
        let was_replaying = self.mode == AppMode::Replaying;
        self.disconnect_external_tool();
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
        reset_preview_render_cache();
        self.last_preview_process_at = None;
        self.with_active_viewer_mut(|viewer| {
            viewer.clear_session_state();
            viewer.line_profile_tool.clear();
            viewer.ruler_tool.clear();
            viewer.annotation_manager = Default::default();
        });
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
        self.sync_config_global_from_runtime();

        let Some(ctrl) = &self.controller else {
            return;
        };

        if self.config_dirty {
            if let Err(e) = self.config.validate(self.sensor_width, self.sensor_height) {
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
        let global_settings = GlobalSettings {
            nm_per_pixel: self.nm_per_pixel,
            sensor_width: self.sensor_width,
            sensor_height: self.sensor_height,
            acq_time_ms: self.published_acq_time_ms(),
            event_store_budget_bytes: self.event_store.memory_budget_bytes(),
        };
        if let Ok(json) = serde_json::to_vec(&global_settings) {
            self.plugin_context_data
                .insert(CTX_GLOBAL_SETTINGS.to_owned(), json);
        }
        let retained_history_needed = self.plugins_need_retained_event_history();
        let runtime_plugins_enabled = self
            .plugin_manager
            .records()
            .iter()
            .any(|record| record.plugin().is_some_and(|plugin| plugin.enabled()));
        let ffi_events = if runtime_plugins_enabled {
            let ffi_events: Vec<FfiCdEvent> = frame
                .events
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|event| FfiCdEvent {
                    timestamp: event.timestamp,
                    x: event.x,
                    y: event.y,
                    polarity: u8::from(event.polarity),
                })
                .collect();
            if retained_history_needed {
                self.event_store.push_frame(
                    &ffi_events,
                    frame.window_start_us,
                    frame.window_end_us,
                );
            } else {
                self.event_store.clear();
            }
            ffi_events
        } else {
            self.event_store.clear();
            Vec::new()
        };

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
                        &ffi_events,
                        &self.event_store,
                        &mut self.analysis_output,
                        &mut self.plugin_context_data,
                        &mut self.persistent_context_data,
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

        let external_streaming = self.external_tool_status().is_streaming();
        let needs_texture = !external_streaming && self.active_view_mode() == ViewMode::Preview2d;
        let force_process =
            (needs_texture && self.texture.is_none()) || self.replay_pause_after_seek_frame;
        let process_interval = self.active_preview_process_interval();
        if !force_process
            && self
                .last_preview_process_at
                .is_some_and(|at| at.elapsed() < process_interval)
        {
            return;
        }

        if self.mode == AppMode::Replaying && self.replay_pause_after_seek_frame {
            if let Some(controls) = &self.replay_controls {
                self.set_replay_paused_internal(controls, true);
            }
            self.replay_pause_after_seek_frame = false;
            self.replay_paused = true;
        }

        self.run_analysis_plugins(&frame);

        let (preview_mode, time_surface_tau_us) =
            self.with_active_viewer(|viewer| (viewer.preview_mode, viewer.time_surface_tau_us));
        let histogram = compute_frame_histogram(&frame, preview_mode, time_surface_tau_us);
        self.apply_preview_histogram(histogram);

        self.with_active_viewer_mut(|viewer| {
            if viewer.line_profile_tool.has_line() {
                viewer.line_profile_tool.recompute(&frame);
            }
        });

        let external_tool_error = if let Some(tool) = &mut self.external_tool {
            tool.send_frame(&frame, self.nm_per_pixel)
                .err()
                .map(|err| format!("{} bridge failed: {err}", tool.name()))
        } else {
            None
        };
        if let Some(err) = external_tool_error {
            self.last_error = Some(err);
            self.disconnect_external_tool();
        }

        if needs_texture {
            let image = self.render_preview_image(&frame);
            if let Some(texture) = &mut self.texture {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                self.texture =
                    Some(ctx.load_texture("preview", image, egui::TextureOptions::LINEAR));
            }
        }
        if let Some(events) = frame.events.as_deref() {
            self.with_active_viewer_mut(|viewer| viewer.workspace.point_cloud.push_events(events));
        }
        self.latest_frame = Some(frame);
        self.last_preview_process_at = Some(Instant::now());
    }

    fn settings_are_locked(&self) -> bool {
        self.mode == AppMode::Replaying
            || (self.mode == AppMode::Recording && self.lock_settings_while_recording)
    }

    fn current_replay_fraction(&self) -> f32 {
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

    fn active_preview_process_interval(&self) -> Duration {
        process_interval_for_view_mode(
            self.active_view_mode(),
            self.effective_preview_interval_ms(),
            self.point_cloud_interval_ms,
        )
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
        self.poll_replay_open_task();
        self.poll_tiff_stack_export_task();
        self.update_preview_texture(ctx);
        self.poll_pipeline_state();
        self.refresh_host_view_registry_if_dirty();

        let mode = self.mode;
        let settings_locked = self.settings_are_locked();
        let mut plugin_toggle_changed = false;
        let mut view_mode_changed = false;
        let mut plugin_scan_requested = false;
        let mut open_plugins_dir_requested = false;
        let mut disconnect_external_tool_requested = false;
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
                        if ui
                            .add_enabled(
                                self.replay_file_info.is_some()
                                    && self.replay_path.is_some()
                                    && self.export_task.is_none(),
                                egui::Button::new("Export TIFF Stack…"),
                            )
                            .clicked()
                        {
                            self.open_tiff_stack_export_dialog();
                            ui.close_menu();
                        }
                        ui.separator();
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
                                let default_name = format!("output_{}.raw", format_timestamp_now());
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
                        self.sync_config_global_from_runtime();
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
                                    let mask_file = cfg
                                        .pixel_mask
                                        .mask_file
                                        .as_ref()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_default();
                                    self.config = cfg;
                                    let global = self.config.global.clone();
                                    self.apply_global_config(&global);
                                    self.mask_file = mask_file;
                                    self.config_dirty = false;
                                    self.acq_dirty = false;
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
                    }
                });

                // ── Settings ────────────────────────────────────────────────
                ui.menu_button("Settings", |ui| {
                    const PIXEL_SCALE_TOOLTIP: &str = "Physical size of one sensor pixel in nanometers. Default is the IMX636 sensor pitch: 4860 nm (4.86 µm). Shared with plugins for coordinate conversion and used by the ruler/scale bar. In optical setups with magnification, replace it with the effective sample-plane calibration.";
                    const SENSOR_DIMENSIONS_TOOLTIP: &str = "Sensor pixel dimensions. Must match the connected camera. Defaults to IMX636 (1280x720). Used for ROI validation and plugin coordinate systems. Only editable when idle.";
                    const ACQ_TIME_TOOLTIP: &str = "Duration of each preview frame's accumulation window. Lower values give finer temporal resolution but fewer events per frame. Higher values integrate more events for a brighter preview but reduce temporal detail.";
                    const EVENT_HISTORY_TOOLTIP: &str = "Maximum memory for retained decoded event history. Plugins can access past frames from this buffer. Increase for longer analysis windows; decrease to save RAM.";
                    const PREVIEW_UPDATE_TOOLTIP: &str = "Maximum redraw interval for the 2D preview. Lower values give a smoother display but higher CPU/GPU load. Does not affect recording or replay timing. Default 33 ms is about 30 fps.";
                    const POINT_CLOUD_UPDATE_TOOLTIP: &str = "Maximum redraw interval for the 3D point cloud view. Lower values are smoother but more GPU-intensive. Default 67 ms is about 15 fps.";
                    const DISK_WRITER_BUFFER_TOOLTIP: &str = "Write buffer size for the recording output file. Larger buffers reduce disk I/O pressure during high-bandwidth recordings. Only editable when idle.";
                    let mut global_settings_changed = false;

                    ui.horizontal(|ui| {
                        ui.label("Pixel scale [nm/px]")
                            .on_hover_text(PIXEL_SCALE_TOOLTIP);
                        let response = ui
                            .add(
                                egui::DragValue::new(&mut self.nm_per_pixel)
                                    .speed(1.0)
                                    .clamp_range(1.0..=10_000.0),
                            )
                            .on_hover_text(PIXEL_SCALE_TOOLTIP);
                        if response.changed() {
                            global_settings_changed = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Sensor").on_hover_text(SENSOR_DIMENSIONS_TOOLTIP);
                        let width_response = ui
                            .add_enabled(
                                mode == AppMode::Idle,
                                egui::DragValue::new(&mut self.sensor_width)
                                    .prefix("w ")
                                    .clamp_range(1..=u16::MAX),
                            )
                            .on_hover_text(SENSOR_DIMENSIONS_TOOLTIP);
                        if width_response.changed() {
                            global_settings_changed = true;
                        }
                        let height_response = ui
                            .add_enabled(
                                mode == AppMode::Idle,
                                egui::DragValue::new(&mut self.sensor_height)
                                    .prefix("h ")
                                    .clamp_range(1..=u16::MAX),
                            )
                            .on_hover_text(SENSOR_DIMENSIONS_TOOLTIP);
                        if height_response.changed() {
                            global_settings_changed = true;
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.label("Acq time [ms]").on_hover_text(ACQ_TIME_TOOLTIP);
                        let acq_time_locked =
                            mode == AppMode::Recording && settings_locked;
                        let response = ui
                            .add_enabled(
                                !acq_time_locked,
                                egui::Slider::new(&mut self.acq_time_ms, 1..=1000),
                            )
                            .on_hover_text(ACQ_TIME_TOOLTIP);
                        if response.changed() {
                            self.acq_dirty = true;
                            global_settings_changed = true;
                        }
                    });

                    let mut event_store_budget_mb = self.event_store_budget_mib();
                    let event_store_response = ui
                        .add(
                            egui::Slider::new(&mut event_store_budget_mb, 1..=1024)
                                .text("Event history budget (MB)"),
                        )
                        .on_hover_text(EVENT_HISTORY_TOOLTIP);
                    if event_store_response.changed() {
                        self.event_store
                            .set_memory_budget(mib_to_bytes(event_store_budget_mb));
                        global_settings_changed = true;
                    }
                    ui.small(format!(
                        "Retained {:.1} MiB across {} frame(s).",
                        self.event_store.memory_usage_bytes() as f32 / EVENT_STORE_MEBIBYTE as f32,
                        self.event_store.frame_count()
                    ));

                    ui.separator();
                    egui::CollapsingHeader::new("Advanced").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let mut preview_hz = interval_ms_to_hz(self.preview_interval_ms);
                            ui.label("Preview update [Hz]")
                                .on_hover_text(PREVIEW_UPDATE_TOOLTIP);
                            let response = ui
                                .add(
                                    egui::DragValue::new(&mut preview_hz)
                                        .clamp_range(5.0..=100.0)
                                        .speed(0.5),
                                )
                                .on_hover_text(PREVIEW_UPDATE_TOOLTIP);
                            if response.changed() {
                                self.preview_interval_ms =
                                    hz_to_interval_ms(preview_hz, 5.0..=100.0);
                                global_settings_changed = true;
                            }
                        });
                        if mode == AppMode::Replaying {
                            ui.small(format!(
                                "Effective display rate: {:.1} Hz (auto from speed × acq time).",
                                interval_ms_to_hz(self.effective_preview_interval_ms())
                            ));
                        }
                        ui.horizontal(|ui| {
                            let mut point_cloud_hz = interval_ms_to_hz(self.point_cloud_interval_ms);
                            ui.label("Point cloud update [Hz]")
                                .on_hover_text(POINT_CLOUD_UPDATE_TOOLTIP);
                            let response = ui
                                .add(
                                    egui::DragValue::new(&mut point_cloud_hz)
                                        .clamp_range(2.0..=50.0)
                                        .speed(0.5),
                                )
                                .on_hover_text(POINT_CLOUD_UPDATE_TOOLTIP);
                            if response.changed() {
                                self.point_cloud_interval_ms =
                                    hz_to_interval_ms(point_cloud_hz, 2.0..=50.0);
                                global_settings_changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Disk writer buffer [MB]")
                                .on_hover_text(DISK_WRITER_BUFFER_TOOLTIP);
                            let response = ui
                                .add_enabled(
                                    mode == AppMode::Idle,
                                    egui::DragValue::new(&mut self.disk_writer_buffer_mib)
                                        .clamp_range(1..=64),
                                )
                                .on_hover_text(DISK_WRITER_BUFFER_TOOLTIP);
                            if response.changed() {
                                global_settings_changed = true;
                            }
                        });
                    });

                    if global_settings_changed {
                        self.sync_config_global_from_runtime();
                    }
                });

                // ── View ──────────────────────────────────────────────────────
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.settings_panel_open, "Settings Panel");
                    ui.checkbox(&mut self.analysis_panel_open, "Analysis Panel");
                    let mut scale_bar_show =
                        self.with_active_viewer(|viewer| viewer.scale_bar_settings.show);
                    if ui.checkbox(&mut scale_bar_show, "Show Scale Bar").changed() {
                        self.with_active_viewer_mut(|viewer| {
                            viewer.scale_bar_settings.show = scale_bar_show;
                        });
                    }
                    let window_views: Vec<ResolvedHostView> =
                        self.host_view_registry.window_views().cloned().collect();
                    for view in window_views {
                        let open = self
                            .host_view_window_open
                            .entry(view.descriptor.id.clone())
                            .or_insert(false);
                        ui.checkbox(open, &view.descriptor.title);
                    }
                    ui.separator();
                    let mut view_mode = self.active_view_mode();
                    let r2d = ui.radio_value(&mut view_mode, ViewMode::Preview2d, "2D Preview");
                    let r3d =
                        ui.radio_value(&mut view_mode, ViewMode::PointCloud3d, "3D Point Cloud");
                    if r2d.changed() || r3d.changed() {
                        view_mode_changed = true;
                        self.with_active_viewer_mut(|viewer| viewer.set_view_mode(view_mode));
                    }
                });

                // ── Tools ─────────────────────────────────────────────────────
                ui.menu_button("Tools", |ui| {
                    match self.external_tool_status() {
                        ExternalToolStatus::Streaming | ExternalToolStatus::Connecting => {
                            if ui.button("Disconnect ImageJ").clicked() {
                                disconnect_external_tool_requested = true;
                                ui.close_menu();
                            }
                        }
                        _ => {
                            if ui.button("Stream to ImageJ...").clicked() {
                                self.imagej_dialog.open = true;
                                ui.close_menu();
                            }
                        }
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
                    let external_status = self.external_tool_status();
                    if !matches!(external_status, ExternalToolStatus::Disconnected) {
                        ui.label(format!("ImageJ: {}", external_status.label()));
                        ui.separator();
                    }
                    let mut view_mode = self.active_view_mode();
                    let r3d = ui.selectable_value(&mut view_mode, ViewMode::PointCloud3d, "3D");
                    let r2d = ui.selectable_value(&mut view_mode, ViewMode::Preview2d, "2D");
                    if r2d.changed() || r3d.changed() {
                        view_mode_changed = true;
                        self.with_active_viewer_mut(|viewer| viewer.set_view_mode(view_mode));
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

        if disconnect_external_tool_requested {
            self.disconnect_external_tool();
        }

        if plugin_toggle_changed {
            self.reset_analysis();
        }

        if ctx.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        }) {
            self.with_active_viewer_mut(|viewer| {
                if let Some(annotation_id) = viewer.annotation_manager.delete_selected() {
                    viewer
                        .workspace
                        .clear_crop_target_if_annotation(annotation_id);
                }
            });
        }

        if settings_locked {
            self.with_active_viewer_mut(|viewer| {
                if viewer.workspace.tool == PreviewTool::SelectRoi {
                    viewer.workspace.clear_selection();
                }
            });
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
                                    self.sensor_width,
                                    self.sensor_height,
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
                .show_separator_line(true)
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
                            for index in 0..self.builtin_plugins.len() {
                                let rendered = {
                                    let plugin = &mut self.builtin_plugins[index];
                                    if !plugin.enabled() {
                                        continue;
                                    }

                                    let missing_dependencies: Vec<&str> = plugin
                                        .dependencies()
                                        .iter()
                                        .copied()
                                        .filter(|dependency| {
                                            !enabled_plugin_names
                                                .iter()
                                                .any(|name| name == dependency)
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

                                    if plugin.ui_settings(
                                        ui,
                                        &mut self.config,
                                        self.sensor_width,
                                        self.sensor_height,
                                    ) {
                                        builtin_plugin_config_changed = true;
                                    }
                                    true
                                };

                                if rendered {
                                    self.render_provider_host_views(
                                        ctx,
                                        ui,
                                        HostViewProviderKey::Builtin(index),
                                    );
                                    ui.separator();
                                }
                            }

                            let runtime_count = self.plugin_manager.records().len();
                            for index in 0..runtime_count {
                                let rendered = {
                                    let record = &mut self.plugin_manager.records_mut()[index];
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
                                            !enabled_plugin_names
                                                .iter()
                                                .any(|name| name == *dependency)
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
                                    true
                                };

                                if rendered {
                                    self.render_provider_host_views(
                                        ctx,
                                        ui,
                                        HostViewProviderKey::Runtime(index),
                                    );
                                    ui.separator();
                                }
                            }
                        });
                });
        } else if !enabled_plugin_names.is_empty() {
            egui::SidePanel::right("analysis-collapsed")
                .exact_width(COLLAPSED_PANEL_WIDTH)
                .resizable(false)
                .show_separator_line(true)
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
            self.host_view_registry_dirty = true;
        }

        let external_status = self.external_tool_status();
        let external_streaming = external_status.is_streaming();
        let external_streaming_label = self.current_external_streaming_label();
        let pipeline_stats = self
            .controller
            .as_ref()
            .map(PipelineController::stats_snapshot);
        let detected_hotpixels = self.latest_detected_hotpixels();
        let mut main_viewer_output = None;
        let mut return_preview_to_main = false;

        if self.popup_open {
            self.sync_popup_shared(
                settings_locked,
                external_streaming,
                &external_streaming_label,
            );
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            if self.popup_open {
                ui.heading("Viewer open in separate window");
                ui.separator();
                let max_image_height = (ui.available_size().y - 56.0).max(180.0);
                draw_text_placeholder(ui, max_image_height, "Preview open in separate window");
                ui.add_space(8.0);
                if ui.button("Return preview to main window").clicked() {
                    return_preview_to_main = true;
                }
            } else {
                let input = ViewerInput {
                    texture: self.texture.as_ref(),
                    frame: self.latest_frame.as_ref(),
                    overlays: &self.analysis_output.overlays,
                    camera_info: self.camera_info.as_ref(),
                    nm_per_pixel: self.nm_per_pixel,
                    config: &self.config,
                    mode: self.mode,
                    settings_locked,
                    pipeline_stats: pipeline_stats.as_ref(),
                    replay: self.viewer_replay_state(),
                    analysis_warnings: &self.analysis_output.warnings,
                    analysis_notice: self.analysis_notice.as_deref(),
                    detected_hotpixels: &detected_hotpixels,
                    config_dirty: self.config_dirty,
                    acq_dirty: self.acq_dirty,
                    replay_open_task_active: self.replay_open_task.is_some(),
                    replay_notice: self.replay_notice.as_deref(),
                    last_error: self.last_error.as_deref(),
                    external_streaming,
                    external_streaming_label: &external_streaming_label,
                    popup_active: false,
                    popup_button_label: "Enlarge",
                    popup_button_tooltip: "Open in separate window",
                    viewer_id: "main",
                };
                main_viewer_output = Some(draw_viewer(ctx, ui, &mut self.viewer, input));
            }
        });

        if return_preview_to_main {
            self.close_popup_viewer();
        }

        if let Some(output) = main_viewer_output {
            self.handle_viewer_output(ctx, output, false);
        }
        if !self.popup_open {
            let contrast_changed = self.viewer.show_aux_windows(ctx);
            if contrast_changed {
                self.refresh_preview_if_needed(ctx, true);
            }
        }
        self.show_imagej_dialog(ctx);
        if let Some(action) = self.export_dialog.show(ctx) {
            match action {
                ExportDialogAction::Export(params) => self.start_tiff_stack_export(params),
                ExportDialogAction::Cancel => {}
            }
        }

        if self.popup_open {
            let shared = Arc::clone(&self.popup_shared);
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("popup_preview"),
                egui::ViewportBuilder::default()
                    .with_title("Preview \u{2014} AugurRS")
                    .with_inner_size([1280.0, 820.0]),
                move |ctx, class| match class {
                    egui::viewport::ViewportClass::Deferred => {
                        let mut root_repaint_requested = false;
                        let mut repaint_after = None;
                        if let Ok(mut data) = shared.lock() {
                            egui::CentralPanel::default().show(ctx, |ui| {
                                let PopupSharedData {
                                    viewer,
                                    texture,
                                    frame,
                                    overlays,
                                    camera_info,
                                    nm_per_pixel,
                                    config,
                                    mode,
                                    settings_locked,
                                    pipeline_stats,
                                    replay,
                                    analysis_warnings,
                                    analysis_notice,
                                    detected_hotpixels,
                                    config_dirty,
                                    acq_dirty,
                                    replay_open_task_active,
                                    replay_notice,
                                    last_error,
                                    external_streaming,
                                    external_streaming_label,
                                    preview_interval_ms,
                                    point_cloud_interval_ms,
                                    close_requested: _,
                                    output,
                                } = &mut *data;
                                let input = ViewerInput {
                                    texture: texture.as_ref(),
                                    frame: frame.as_ref(),
                                    overlays,
                                    camera_info: camera_info.as_ref(),
                                    nm_per_pixel: *nm_per_pixel,
                                    config,
                                    mode: *mode,
                                    settings_locked: *settings_locked,
                                    pipeline_stats: pipeline_stats.as_ref(),
                                    replay: *replay,
                                    analysis_warnings,
                                    analysis_notice: analysis_notice.as_deref(),
                                    detected_hotpixels,
                                    config_dirty: *config_dirty,
                                    acq_dirty: *acq_dirty,
                                    replay_open_task_active: *replay_open_task_active,
                                    replay_notice: replay_notice.as_deref(),
                                    last_error: last_error.as_deref(),
                                    external_streaming: *external_streaming,
                                    external_streaming_label: external_streaming_label.as_str(),
                                    popup_active: true,
                                    popup_button_label: "Return to augur",
                                    popup_button_tooltip: "Return viewer to main window",
                                    viewer_id: "popup",
                                };
                                let mut popup_output = draw_viewer(ctx, ui, viewer, input);
                                if viewer.show_aux_windows(ctx) {
                                    popup_output.contrast_changed = true;
                                }
                                root_repaint_requested = popup_output.requests_root_update();
                                output
                                    .get_or_insert_with(Default::default)
                                    .merge(popup_output);
                                repaint_after = child_viewport_repaint_after(
                                    viewport_stream_active(*mode, *replay),
                                    *replay_open_task_active,
                                    process_interval_for_view_mode(
                                        viewer.view_mode,
                                        *preview_interval_ms,
                                        *point_cloud_interval_ms,
                                    ),
                                    *preview_interval_ms,
                                );
                            });
                        }
                        if let Some(duration) = repaint_after {
                            ctx.request_repaint_after(duration);
                        }
                        if root_repaint_requested {
                            request_root_repaint(ctx);
                        }
                        if ctx.input(|i| i.viewport().close_requested()) {
                            if let Ok(mut d) = shared.lock() {
                                d.close_requested = true;
                            }
                            request_root_repaint(ctx);
                        }
                    }
                    egui::viewport::ViewportClass::Embedded => {
                        let mut open = true;
                        egui::Window::new("Preview \u{2014} AugurRS")
                            .open(&mut open)
                            .default_size([1100.0, 760.0])
                            .show(ctx, |ui| {
                                if let Ok(mut data) = shared.lock() {
                                    let PopupSharedData {
                                        viewer,
                                        texture,
                                        frame,
                                        overlays,
                                        camera_info,
                                        nm_per_pixel,
                                        config,
                                        mode,
                                        settings_locked,
                                        pipeline_stats,
                                        replay,
                                        analysis_warnings,
                                        analysis_notice,
                                        detected_hotpixels,
                                        config_dirty,
                                        acq_dirty,
                                        replay_open_task_active,
                                        replay_notice,
                                        last_error,
                                        external_streaming,
                                        external_streaming_label,
                                        preview_interval_ms: _,
                                        point_cloud_interval_ms: _,
                                        close_requested: _,
                                        output,
                                    } = &mut *data;
                                    let input = ViewerInput {
                                        texture: texture.as_ref(),
                                        frame: frame.as_ref(),
                                        overlays,
                                        camera_info: camera_info.as_ref(),
                                        nm_per_pixel: *nm_per_pixel,
                                        config,
                                        mode: *mode,
                                        settings_locked: *settings_locked,
                                        pipeline_stats: pipeline_stats.as_ref(),
                                        replay: *replay,
                                        analysis_warnings,
                                        analysis_notice: analysis_notice.as_deref(),
                                        detected_hotpixels,
                                        config_dirty: *config_dirty,
                                        acq_dirty: *acq_dirty,
                                        replay_open_task_active: *replay_open_task_active,
                                        replay_notice: replay_notice.as_deref(),
                                        last_error: last_error.as_deref(),
                                        external_streaming: *external_streaming,
                                        external_streaming_label: external_streaming_label.as_str(),
                                        popup_active: true,
                                        popup_button_label: "Return to augur",
                                        popup_button_tooltip: "Return viewer to main window",
                                        viewer_id: "popup",
                                    };
                                    let mut popup_output = draw_viewer(ctx, ui, viewer, input);
                                    if viewer.show_aux_windows(ctx) {
                                        popup_output.contrast_changed = true;
                                    }
                                    if popup_output.requests_root_update() {
                                        request_root_repaint(ctx);
                                    }
                                    output
                                        .get_or_insert_with(Default::default)
                                        .merge(popup_output);
                                }
                            });
                        if !open {
                            if let Ok(mut d) = shared.lock() {
                                d.close_requested = true;
                            }
                            request_root_repaint(ctx);
                        }
                    }
                    _ => {}
                },
            );

            let (close, popup_output) = {
                let mut data = self.popup_shared.lock().unwrap();
                let close = data.close_requested;
                data.close_requested = false;
                (close, data.output.take())
            };
            if close {
                self.close_popup_viewer();
            }
            if let Some(output) = popup_output {
                self.handle_viewer_output(ctx, output, true);
            }
        }

        self.render_host_view_windows(ctx);

        self.sync_active_pipeline_requirements();

        let stream_active = matches!(self.mode, AppMode::Previewing | AppMode::Recording)
            || (self.mode == AppMode::Replaying && !self.replay_paused && !self.replay_finished);
        let process_interval = self.active_preview_process_interval();
        if self.replay_open_task.is_some() {
            ctx.request_repaint_after(Duration::from_millis(self.preview_interval_ms));
        } else if stream_active && self.controller.is_some() {
            ctx.request_repaint_after(process_interval);
        }
    }
}

fn status_success_color() -> egui::Color32 {
    egui::Color32::from_rgb(0, 160, 60)
}

fn analysis_info_color() -> egui::Color32 {
    egui::Color32::from_rgb(30, 100, 220)
}

fn replay_pipeline_config(info: &ReplayFileInfo) -> CameraConfig {
    let mut config = CameraConfig::default();
    config.roi.width = info.width;
    config.roi.height = info.height;
    config.global.sensor_width = info.width;
    config.global.sensor_height = info.height;
    if let Some(pixel_pitch_nm) = info.metadata.pixel_pitch_nm {
        config.global.nm_per_pixel = pixel_pitch_nm;
    }
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

fn replay_speed_matches(current: f32, candidate: f32) -> bool {
    if current.is_infinite() || candidate.is_infinite() {
        current.is_infinite() && candidate.is_infinite()
    } else {
        (current - candidate).abs() < f32::EPSILON
    }
}

fn sanitize_file_stem(title: &str) -> String {
    let mut stem = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            stem.push(ch.to_ascii_lowercase());
        } else if !stem.ends_with('_') {
            stem.push('_');
        }
    }

    let stem = stem.trim_matches('_').to_owned();
    if stem.is_empty() {
        "host_view".into()
    } else {
        stem
    }
}

fn ensure_extension(mut path: PathBuf, extension: &str) -> PathBuf {
    if path.extension().is_none() {
        path.set_extension(extension);
    }
    path
}

impl Drop for CameraApp {
    fn drop(&mut self) {
        self.stop_pipeline();
    }
}

#[cfg(test)]
mod tests {
    use super::derived_replay_preview_interval_ms;

    #[test]
    fn replay_preview_interval_scales_with_speed() {
        assert_eq!(derived_replay_preview_interval_ms(50, 1.0), 50);
        assert_eq!(derived_replay_preview_interval_ms(50, 2.0), 25);
        assert_eq!(derived_replay_preview_interval_ms(50, 0.5), 100);
    }

    #[test]
    fn replay_preview_interval_clamps_extremes() {
        assert_eq!(derived_replay_preview_interval_ms(1, 4.0), 10);
        assert_eq!(derived_replay_preview_interval_ms(1_000, 0.25), 200);
        assert_eq!(derived_replay_preview_interval_ms(50, f32::INFINITY), 10);
    }
}
