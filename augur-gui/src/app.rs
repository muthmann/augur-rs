use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime},
};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, Overlay},
    camera::{DeviceInfo, EventCamera},
    config::{CameraConfig, GlobalSettingsConfig, RoiConfig},
    pipeline::{
        spawn_pipeline, CdEvent, Evt3CorePreviewDecoder, PipelineController, PipelineOptions,
        PreviewFrame,
    },
    replay::{align_relative_evt3_word_offset, RawFileCamera, ReplayControls, ReplayFileInfo},
    DecodedEventFileCamera, PackedEventPreviewDecoder, PACKED_EVENT_RECORD_BYTES,
};
use augur_plugin_api::{
    EventStore, FfiCdEvent, GlobalSettings, HostViewKind, PluginInput, TableDatasetV1,
    CTX_GLOBAL_SETTINGS,
};
use augur_prophesee::evk4::Evk4Camera;
use egui_phosphor::regular as phosphor;

use crate::{
    colormap::Colormap,
    external_tools::{
        ExternalTool, ExternalToolStatus, ImageJBridge, BUNDLED_IMAGEJ_PLUGIN_JAR,
        BUNDLED_IMAGEJ_PLUGIN_JAR_NAME, DEFAULT_IMAGEJ_BRIDGE_PORT,
    },
    host_views::{
        decode_dataset_snapshot, export_image_to_path, export_table_csv_to_path,
        render_compact_table, render_density2d_view, render_density_window_viewport,
        render_table_window, render_table_window_viewport, reset_provider_for_dataset,
        resolve_host_view_registry, DensityWindowViewportData, HostDatasetSnapshot,
        HostRegistryContribution, HostViewImageFormat, HostViewProviderKey, HostViewRenderState,
        HostViewUiActions, ResolvedHostView, ResolvedHostViewRegistry, TableWindowViewportData,
    },
    plugin::AnalysisPlugin,
    plugin_loader::PluginManager,
    plugin_settings_ui::render_plugin_settings,
    plugins::create_all_plugins,
    point_cloud::{PointCloudMetrics, PointCloudState},
    preview::{compute_frame_histogram, frame_to_color_image, PreviewDisplaySettings},
    settings::draw_settings,
    viewer_tools::{
        compute_scale_bar, AnnotationManager, AnnotationShape, AnnotationShapeKind, ContrastMode,
        ContrastSettings, HistogramWindow, LineProfileTool, RulerTool, ScaleBarPosition,
        ScaleBarSettings,
    },
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
const EVENT_STORE_MEBIBYTE: usize = 1024 * 1024;
pub(crate) const PANEL_ROUNDING: f32 = 6.0;
const PREVIEW_ZOOM_MIN: f32 = 1.0;
const PREVIEW_ZOOM_MAX: f32 = 16.0;

type CachedHostDataset = Result<Option<HostDatasetSnapshot>, String>;

fn mib_to_bytes(mib: u64) -> usize {
    mib.saturating_mul(EVENT_STORE_MEBIBYTE as u64)
        .min(usize::MAX as u64) as usize
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
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Preview2d,
    PointCloud3d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewTool {
    None,
    SelectRoi,
    LineProfile,
    Ruler,
    AnnotateRect,
    AnnotateEllipse,
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
    replay_seek_drag_value: Option<f32>,
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
    preview_colormap: Option<Colormap>,
    contrast_settings: ContrastSettings,
    histogram_window: HistogramWindow,
    line_profile_tool: LineProfileTool,
    ruler_tool: RulerTool,
    scale_bar_settings: ScaleBarSettings,
    annotation_manager: AnnotationManager,
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
    imagej_dialog: ImageJDialogState,
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
            replay_seek_drag_value: None,
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
            preview_colormap: None,
            contrast_settings: ContrastSettings::default(),
            histogram_window: HistogramWindow::default(),
            line_profile_tool: LineProfileTool::default(),
            ruler_tool: RulerTool::default(),
            scale_bar_settings: ScaleBarSettings::default(),
            annotation_manager: AnnotationManager::default(),
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
            imagej_dialog: ImageJDialogState::default(),
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
                    ui.label(
                        egui::RichText::new("Status:")
                            .size(body_size),
                    );
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
                    ui.label(
                        egui::RichText::new(format!("{}. {step}", i + 1))
                            .size(small_size),
                    );
                }

                ui.add_space(6.0 * scale);
                if ui
                    .add(egui::Button::new(
                        egui::RichText::new("Save AugurBridge.jar...")
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
                            egui::Button::new(
                                egui::RichText::new("Connect").size(body_size),
                            ),
                        )
                        .clicked()
                    {
                        connect_requested = true;
                    }
                    if ui
                        .add_enabled(
                            self.external_tool.is_some(),
                            egui::Button::new(
                                egui::RichText::new("Disconnect").size(body_size),
                            ),
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

        match export_table_csv_to_path(&path, dataset.descriptor.kind.table_schema(), table) {
            Ok(()) => self.last_error = None,
            Err(err) => self.last_error = Some(err),
        }
    }

    fn export_host_view_image(&mut self, view: &ResolvedHostView, format: HostViewImageFormat) {
        let image = match self.host_view_render_state.get(&view.descriptor.id) {
            Some(HostViewRenderState::Density2d(state)) => state.image(),
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
        let schema = dataset.descriptor.kind.table_schema();
        let cache_entry = self
            .host_view_dataset_cache
            .get(&view.descriptor.dataset_id);

        match &view.descriptor.kind {
            HostViewKind::CompactTable => match cached_table(cache_entry) {
                Ok(table) => {
                    render_compact_table(ui, schema, table, &dataset.descriptor.empty_message);
                }
                Err(err) => {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
            },
            HostViewKind::TableWindow => {
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
            HostViewKind::Density2dFromTable { x_column, y_column } => {
                let actions = {
                    let state = self
                        .host_view_render_state
                        .entry(view.descriptor.id.clone())
                        .or_default()
                        .density_state();
                    let dataset_generation = cache_entry.map(|entry| entry.generation).unwrap_or(0);

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

            match &view.descriptor.kind {
                HostViewKind::TableWindow => {
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
                        schema: dataset.descriptor.kind.table_schema().clone(),
                        dataset: table_arc,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_csv_requested: false,
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&title)
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
                                egui::Window::new(&title)
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
                HostViewKind::Density2dFromTable { x_column, y_column } => {
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

                        let table = cached_table(cache_entry.as_ref()).unwrap_or(None);

                        let render_result = state.render_if_needed(
                            ctx,
                            dataset.descriptor.kind.table_schema(),
                            table,
                            dataset_generation,
                            x_column,
                            y_column,
                        );
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
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&title)
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
                                egui::Window::new(&title)
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
                HostViewKind::CompactTable => {}
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
        if self.mode != AppMode::Idle || self.replay_open_task.is_some() {
            return;
        }
        match self.start_pipeline_inner(true) {
            Ok(controller) => {
                self.sync_pipeline_requirements(&controller);
                self.controller = Some(controller);
                self.last_preview_process_at = None;
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
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::default(),
                        replay_pipeline_config(&info),
                        PipelineOptions::preview_only(info.width, info.height),
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
                            spawn_pipeline(
                                camera,
                                PackedEventPreviewDecoder::default(),
                                replay_pipeline_config(&info),
                                PipelineOptions::preview_only(info.width, info.height),
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
            self.load_replay_display_settings(&path, info.width, info.height);

        self.set_replay_paused_internal(&controls, false);
        self.set_replay_speed_internal(&controls, 1.0);
        self.sync_pipeline_requirements(&controller);
        self.controller = Some(controller);
        self.last_preview_process_at = None;
        self.mode = AppMode::Replaying;
        self.preview_workspace.clear_session_state();
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

    fn load_replay_display_settings(
        &self,
        raw_path: &Path,
        width: u16,
        height: u16,
    ) -> (CameraConfig, String, Option<String>) {
        let mut default_config = CameraConfig::default();
        default_config.roi.width = width;
        default_config.roi.height = height;
        default_config.global.sensor_width = width;
        default_config.global.sensor_height = height;

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
                    config.global.sensor_width = width;
                    config.global.sensor_height = height;
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
        PreviewDisplaySettings {
            display_min: self.contrast_settings.display_min,
            display_max: self.contrast_settings.display_max,
            gamma: self.contrast_settings.gamma,
        }
    }

    fn render_preview_image(&self, frame: &PreviewFrame) -> egui::ColorImage {
        frame_to_color_image(
            frame,
            &self.analysis_output.overlays,
            self.preview_display_settings(),
            self.preview_colormap,
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

    fn refresh_paused_preview_if_needed(&mut self, ctx: &egui::Context, settings_changed: bool) {
        if !settings_changed || self.preview_workspace.view_mode != ViewMode::Preview2d {
            return;
        }
        if self.mode == AppMode::Replaying && (self.replay_paused || self.replay_finished) {
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
        self.last_preview_process_at = None;
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
        self.replay_seek_drag_value = None;
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
        self.last_preview_process_at = None;
        self.preview_workspace.clear_session_state();
        self.line_profile_tool.clear();
        self.ruler_tool.clear();
        self.annotation_manager = AnnotationManager::default();
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
            self.event_store
                .push_frame(&ffi_events, frame.window_start_us, frame.window_end_us);
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
        let needs_texture = (!external_streaming
            && self.preview_workspace.view_mode == ViewMode::Preview2d)
            || self.preview_workspace.popup_open;
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

        let histogram = compute_frame_histogram(&frame);
        if self.contrast_settings.mode == ContrastMode::Auto {
            self.contrast_settings.update_auto_range(&histogram);
        }
        let histogram_max = histogram.len().saturating_sub(1).min(u16::MAX as usize) as u16;
        let clamped_min = self
            .contrast_settings
            .display_min
            .min(histogram_max.saturating_sub(1));
        let clamped_max = self
            .contrast_settings
            .display_max
            .clamp(clamped_min.saturating_add(1), histogram_max.max(1));
        self.contrast_settings.display_min = clamped_min;
        self.contrast_settings.display_max = clamped_max;
        self.histogram_window.set_histogram(histogram);

        if self.line_profile_tool.has_line() {
            self.line_profile_tool.recompute(&frame);
        }

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
            self.preview_workspace.point_cloud.push_events(events);
        }
        self.latest_frame = Some(frame);
        self.last_preview_process_at = Some(Instant::now());
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

    fn active_preview_process_interval(&self) -> Duration {
        if self.preview_workspace.view_mode == ViewMode::PointCloud3d {
            Duration::from_millis(self.point_cloud_interval_ms)
        } else {
            Duration::from_millis(self.preview_interval_ms)
        }
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
        self.poll_replay_open_task();
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
                    const PIXEL_SCALE_TOOLTIP: &str = "Physical size of one sensor pixel in nanometers. Shared with plugins for coordinate conversion (for example localization in nm). Typical values: 15 nm (high-mag TIRF), 65 nm (standard SMLM), 100+ nm (wide-field).";
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
                        let response = ui
                            .add_enabled(
                                mode != AppMode::Replaying && !settings_locked,
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
                            ui.label("Preview update [ms]")
                                .on_hover_text(PREVIEW_UPDATE_TOOLTIP);
                            let response = ui
                                .add(
                                    egui::DragValue::new(&mut self.preview_interval_ms)
                                        .clamp_range(10..=200),
                                )
                                .on_hover_text(PREVIEW_UPDATE_TOOLTIP);
                            if response.changed() {
                                global_settings_changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Point cloud update [ms]")
                                .on_hover_text(POINT_CLOUD_UPDATE_TOOLTIP);
                            let response = ui
                                .add(
                                    egui::DragValue::new(&mut self.point_cloud_interval_ms)
                                        .clamp_range(20..=500),
                                )
                                .on_hover_text(POINT_CLOUD_UPDATE_TOOLTIP);
                            if response.changed() {
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
                    ui.checkbox(&mut self.scale_bar_settings.show, "Show Scale Bar");
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
                    let mut view_mode = self.preview_workspace.view_mode;
                    let r2d = ui.radio_value(&mut view_mode, ViewMode::Preview2d, "2D Preview");
                    let r3d =
                        ui.radio_value(&mut view_mode, ViewMode::PointCloud3d, "3D Point Cloud");
                    if r2d.changed() || r3d.changed() {
                        view_mode_changed = true;
                        self.preview_workspace.set_view_mode(view_mode);
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

        if disconnect_external_tool_requested {
            self.disconnect_external_tool();
        }

        if plugin_toggle_changed {
            self.reset_analysis();
        }

        if ctx.input(|input| {
            input.key_pressed(egui::Key::Delete) || input.key_pressed(egui::Key::Backspace)
        }) {
            self.annotation_manager.delete_selected();
        }

        if settings_locked && self.preview_workspace.tool == PreviewTool::SelectRoi {
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
        let mut return_from_external_tool = false;
        let mut pc_metrics: Option<PointCloudMetrics> = None;
        let mut preview_colormap_changed = false;
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

            if external_streaming {
                draw_preview_toolbar(
                    ui,
                    PreviewToolbarState {
                        workspace: &mut self.preview_workspace,
                        latest_frame: self.latest_frame.as_ref(),
                        line_profile_tool: &mut self.line_profile_tool,
                        ruler_tool: &mut self.ruler_tool,
                        nm_per_pixel: self.nm_per_pixel,
                        settings_locked,
                        histogram_open: &mut self.histogram_window.open,
                        popup_button_tooltip: "Open in separate window",
                    },
                );
                let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
                draw_text_placeholder(
                    ui,
                    max_image_height,
                    &format!(
                        "Streaming to ImageJ ({}:{})",
                        self.imagej_dialog.host, self.imagej_dialog.port
                    ),
                );
                ui.add_space(8.0);
                if ui.button("Return to augur").clicked() {
                    return_from_external_tool = true;
                }
            } else if self.preview_workspace.popup_open {
                // When the popup is open, show a placeholder in the main window.
                draw_preview_toolbar(
                    ui,
                    PreviewToolbarState {
                        workspace: &mut self.preview_workspace,
                        latest_frame: self.latest_frame.as_ref(),
                        line_profile_tool: &mut self.line_profile_tool,
                        ruler_tool: &mut self.ruler_tool,
                        nm_per_pixel: self.nm_per_pixel,
                        settings_locked,
                        histogram_open: &mut self.histogram_window.open,
                        popup_button_tooltip: "Open in separate window",
                    },
                );
                let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
                draw_text_placeholder(ui, max_image_height, "Preview open in separate window");
            } else {
                match self.preview_workspace.view_mode {
                    ViewMode::Preview2d => {
                        draw_preview_toolbar(
                            ui,
                            PreviewToolbarState {
                                workspace: &mut self.preview_workspace,
                                latest_frame: self.latest_frame.as_ref(),
                                line_profile_tool: &mut self.line_profile_tool,
                                ruler_tool: &mut self.ruler_tool,
                                nm_per_pixel: self.nm_per_pixel,
                                settings_locked,
                                histogram_open: &mut self.histogram_window.open,
                                popup_button_tooltip: "Open in separate window",
                            },
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
                                PreviewCanvasState {
                                    config: &mut self.config,
                                    workspace: &mut self.preview_workspace,
                                    line_profile_tool: &mut self.line_profile_tool,
                                    ruler_tool: &mut self.ruler_tool,
                                    annotation_manager: &mut self.annotation_manager,
                                },
                                PreviewCanvasOptions {
                                    scale_bar_settings: &self.scale_bar_settings,
                                    nm_per_pixel: self.nm_per_pixel,
                                    settings_locked,
                                    max_height: max_image_height,
                                },
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
                                let previous_preview_colormap = self.preview_colormap;
                                ui.label("Colormap");
                                egui::ComboBox::from_id_source("preview_colormap")
                                    .selected_text(
                                        self.preview_colormap
                                            .map(Colormap::label)
                                            .unwrap_or("Polarity (R/G)"),
                                    )
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut self.preview_colormap,
                                            None,
                                            "Polarity (R/G)",
                                        );
                                        for colormap in Colormap::ALL {
                                            ui.selectable_value(
                                                &mut self.preview_colormap,
                                                Some(colormap),
                                                colormap.label(),
                                            );
                                        }
                                    });
                                preview_colormap_changed |=
                                    self.preview_colormap != previous_preview_colormap;
                                ui.checkbox(
                                    &mut self.scale_bar_settings.show,
                                    "Scale bar",
                                );
                                if self.scale_bar_settings.show {
                                    egui::ComboBox::from_id_source("scale_bar_position")
                                        .selected_text(match self.scale_bar_settings.position {
                                            ScaleBarPosition::TopLeft => "Top left",
                                            ScaleBarPosition::TopRight => "Top right",
                                            ScaleBarPosition::BottomLeft => "Bottom left",
                                            ScaleBarPosition::BottomRight => "Bottom right",
                                        })
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(
                                                &mut self.scale_bar_settings.position,
                                                ScaleBarPosition::TopLeft,
                                                "Top left",
                                            );
                                            ui.selectable_value(
                                                &mut self.scale_bar_settings.position,
                                                ScaleBarPosition::TopRight,
                                                "Top right",
                                            );
                                            ui.selectable_value(
                                                &mut self.scale_bar_settings.position,
                                                ScaleBarPosition::BottomLeft,
                                                "Bottom left",
                                            );
                                            ui.selectable_value(
                                                &mut self.scale_bar_settings.position,
                                                ScaleBarPosition::BottomRight,
                                                "Bottom right",
                                            );
                                        });
                                }
                            });
                            if let Some(stats) = self
                                .latest_frame
                                .as_ref()
                                .and_then(|frame| self.annotation_manager.statistics_for_selected(frame))
                            {
                                ui.small(format!(
                                    "{} | {} px | ON {:.2}±{:.2} | OFF {:.2}±{:.2} | Total {:.2}±{:.2}",
                                    stats.label,
                                    stats.pixel_count,
                                    stats.on.mean,
                                    stats.on.stddev,
                                    stats.off.mean,
                                    stats.off.stddev,
                                    stats.combined.mean,
                                    stats.combined.stddev,
                                ));
                            }
                            if !self.annotation_manager.annotations().is_empty() {
                                egui::CollapsingHeader::new("Annotations")
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        for annotation in self.annotation_manager.annotations() {
                                            let selected = self.annotation_manager.selected_id()
                                                == Some(annotation.id);
                                            let _ = ui.selectable_label(selected, &annotation.label);
                                        }
                                        if ui
                                            .add_enabled(
                                                self.annotation_manager.selected_id().is_some(),
                                                egui::Button::new("Delete selected"),
                                            )
                                            .clicked()
                                        {
                                            self.annotation_manager.delete_selected();
                                        }
                                    });
                            }
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
                        ui.small(format!(
                            "Preview drops: {} packets / {} frames  |  Preview queues HW: {} / {}  |  Disk queue HW: {}  |  Disk wait/write: {:.1} / {:.1} ms",
                            s.preview_packet_drops,
                            s.preview_frame_drops,
                            s.preview_packet_queue_high_water,
                            s.preview_frame_queue_high_water,
                            s.disk_queue_high_water,
                            s.disk_send_wait_us as f64 / 1_000.0,
                            s.disk_write_us as f64 / 1_000.0,
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

                    if self.replay_open_task.is_some() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(
                                self.replay_notice
                                    .as_deref()
                                    .unwrap_or("Opening replay..."),
                            );
                        });
                    }

                    if let Some(err) = &self.last_error {
                        ui.separator();
                        ui.colored_label(ui.visuals().error_fg_color, err);
                    }
                });
        });

        if return_from_external_tool {
            self.disconnect_external_tool();
        }

        let previous_contrast_settings = self.contrast_settings.clone();
        self.histogram_window
            .show(ctx, &mut self.contrast_settings, self.preview_colormap);
        let contrast_changed = self.contrast_settings != previous_contrast_settings;
        self.refresh_paused_preview_if_needed(ctx, preview_colormap_changed || contrast_changed);
        self.line_profile_tool.show_window(ctx);
        self.show_imagej_dialog(ctx);

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

struct PreviewToolbarState<'a> {
    workspace: &'a mut PreviewWorkspaceState,
    latest_frame: Option<&'a PreviewFrame>,
    line_profile_tool: &'a mut LineProfileTool,
    ruler_tool: &'a mut RulerTool,
    nm_per_pixel: f64,
    settings_locked: bool,
    histogram_open: &'a mut bool,
    popup_button_tooltip: &'a str,
}

fn draw_preview_toolbar(ui: &mut egui::Ui, state: PreviewToolbarState<'_>) {
    let PreviewToolbarState {
        workspace,
        latest_frame,
        line_profile_tool,
        ruler_tool,
        nm_per_pixel,
        settings_locked,
        histogram_open,
        popup_button_tooltip,
    } = state;
    ui.horizontal(|ui| {
        if ui
            .add(egui::SelectableLabel::new(
                workspace.tool == PreviewTool::None,
                toolbar_icon(phosphor::CURSOR),
            ))
            .on_hover_text("Pointer")
            .clicked()
        {
            workspace.tool = PreviewTool::None;
            workspace.selection_anchor = None;
            workspace.pending_roi = None;
        }

        let mut select_roi_button = ui.add_enabled(
            !settings_locked,
            egui::SelectableLabel::new(
                workspace.tool == PreviewTool::SelectRoi,
                toolbar_icon(phosphor::SELECTION),
            ),
        );
        if settings_locked {
            select_roi_button = select_roi_button.on_hover_text(
                "Hardware ROI editing is disabled during replay. Use the rectangle annotation tool instead.",
            );
        } else {
            select_roi_button = select_roi_button.on_hover_text("Select hardware ROI");
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

        if preview_tool_button(
            ui,
            workspace,
            PreviewTool::LineProfile,
            phosphor::LINE_SEGMENT,
            "Line profile",
        ) {
            line_profile_tool.clear();
        }
        if preview_tool_button(
            ui,
            workspace,
            PreviewTool::Ruler,
            phosphor::RULER,
            "Measure distance",
        ) {
            ruler_tool.clear();
        }
        preview_tool_button(
            ui,
            workspace,
            PreviewTool::AnnotateRect,
            phosphor::RECTANGLE,
            "Rectangle annotation",
        );
        preview_tool_button(
            ui,
            workspace,
            PreviewTool::AnnotateEllipse,
            phosphor::CIRCLE,
            "Ellipse annotation",
        );

        ui.separator();
        if ui
            .small_button(toolbar_icon(phosphor::MAGNIFYING_GLASS_MINUS))
            .on_hover_text("Zoom out")
            .clicked()
        {
            workspace.zoom = (workspace.zoom / 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
            if (workspace.zoom - PREVIEW_ZOOM_MIN).abs() < f32::EPSILON {
                workspace.pan = egui::Vec2::ZERO;
            }
        }
        if ui
            .small_button(toolbar_icon(phosphor::MAGNIFYING_GLASS_PLUS))
            .on_hover_text("Zoom in")
            .clicked()
        {
            workspace.zoom = (workspace.zoom * 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
        }
        if ui
            .small_button(toolbar_icon(phosphor::FRAME_CORNERS))
            .on_hover_text("Fit to window")
            .clicked()
        {
            workspace.reset_zoom();
        }

        if ui
            .add(egui::SelectableLabel::new(
                workspace.crop_to_roi,
                toolbar_icon(phosphor::CROP),
            ))
            .on_hover_text("Crop to ROI")
            .clicked()
        {
            workspace.crop_to_roi = !workspace.crop_to_roi;
        }

        ui.separator();
        if ui
            .add(egui::SelectableLabel::new(
                *histogram_open,
                toolbar_icon(phosphor::CHART_BAR),
            ))
            .on_hover_text("Histogram & Brightness/Contrast")
            .clicked()
        {
            *histogram_open = !*histogram_open;
        }

        if ui
            .add(egui::SelectableLabel::new(
                workspace.popup_open,
                toolbar_icon(phosphor::ARROW_SQUARE_OUT),
            ))
            .on_hover_text(popup_button_tooltip)
            .clicked()
        {
            workspace.popup_open = !workspace.popup_open;
        }

        ui.separator();
        if let (Some((x, y)), Some(frame)) = (workspace.hover_sensor, latest_frame) {
            let width = usize::from(frame.width.max(1));
            let idx = usize::from(y) * width + usize::from(x);
            if idx < frame.pixels.len() {
                ui.monospace(format!(
                    "x {x}, y {y} | ON: {} OFF: {} Total: {}",
                    frame.pixels_on[idx], frame.pixels_off[idx], frame.pixels[idx]
                ));
            } else {
                ui.weak("Hover preview for pixel values");
            }
        } else {
            ui.weak("Hover preview for pixel values");
        }
        if workspace.tool == PreviewTool::Ruler {
            if let Some(measurement) = ruler_tool.measurement(nm_per_pixel) {
                ui.separator();
                ui.small(format!(
                    "{:.1} px | {:.2} µm",
                    measurement.pixel_distance, measurement.micrometers
                ));
            }
        }
    });
}

fn preview_tool_button(
    ui: &mut egui::Ui,
    workspace: &mut PreviewWorkspaceState,
    tool: PreviewTool,
    icon: &str,
    tooltip: &str,
) -> bool {
    if ui
        .add(egui::SelectableLabel::new(
            workspace.tool == tool,
            toolbar_icon(icon),
        ))
        .on_hover_text(tooltip)
        .clicked()
    {
        if workspace.tool == tool {
            workspace.tool = PreviewTool::None;
            return true;
        } else {
            workspace.tool = tool;
            workspace.selection_anchor = None;
            workspace.pending_roi = None;
        }
    }
    false
}

fn toolbar_icon(symbol: &str) -> egui::RichText {
    egui::RichText::new(symbol).size(18.0)
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
    draw_text_placeholder(ui, max_image_height, empty_preview_message(mode, view_mode));
}

fn draw_text_placeholder(ui: &mut egui::Ui, max_image_height: f32, message: &str) {
    let placeholder_height = max_image_height;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), placeholder_height),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect.intersect(ui.clip_rect()));
    painter.rect_filled(rect, PANEL_ROUNDING, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        PANEL_ROUNDING,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        message,
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

struct PreviewCanvasState<'a> {
    config: &'a mut CameraConfig,
    workspace: &'a mut PreviewWorkspaceState,
    line_profile_tool: &'a mut LineProfileTool,
    ruler_tool: &'a mut RulerTool,
    annotation_manager: &'a mut AnnotationManager,
}

struct PreviewCanvasOptions<'a> {
    scale_bar_settings: &'a ScaleBarSettings,
    nm_per_pixel: f64,
    settings_locked: bool,
    max_height: f32,
}

fn draw_preview_canvas(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    frame: &PreviewFrame,
    state: PreviewCanvasState<'_>,
    options: PreviewCanvasOptions<'_>,
) -> bool {
    let PreviewCanvasState {
        config,
        workspace,
        line_profile_tool,
        ruler_tool,
        annotation_manager,
    } = state;
    let PreviewCanvasOptions {
        scale_bar_settings,
        nm_per_pixel,
        settings_locked,
        max_height,
    } = options;
    let viewport = build_preview_viewport(frame, config, workspace);
    let canvas_size = egui::vec2(ui.available_width().max(1.0), max_height.max(1.0));
    let (canvas_rect, response) =
        ui.allocate_exact_size(canvas_size, egui::Sense::click_and_drag());
    let display_size = viewport.display_size(canvas_rect.size());
    let image_rect = egui::Align2::CENTER_CENTER.align_size_within_rect(display_size, canvas_rect);
    ui.painter()
        .rect_filled(canvas_rect, 4.0, ui.visuals().faint_bg_color);
    egui::Image::new(texture)
        .uv(viewport.uv_rect(frame))
        .paint_at(ui, image_rect);

    workspace.hover_sensor = response
        .hover_pos()
        .and_then(|pos| viewport.screen_to_sensor(image_rect, pos));
    let pointer_sensor = response
        .interact_pointer_pos()
        .and_then(|pos| viewport.screen_to_sensor(image_rect, pos));

    if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        match workspace.tool {
            PreviewTool::SelectRoi => {
                workspace.selection_anchor = None;
                workspace.pending_roi = None;
            }
            PreviewTool::LineProfile => line_profile_tool.clear(),
            PreviewTool::Ruler => ruler_tool.clear(),
            PreviewTool::AnnotateRect | PreviewTool::AnnotateEllipse => {
                annotation_manager.cancel_drawing();
            }
            PreviewTool::None => {}
        }
        workspace.tool = PreviewTool::None;
    }

    let mut roi_committed = false;
    if workspace.tool == PreviewTool::SelectRoi && !settings_locked {
        if response.drag_started() {
            if let Some(pointer_pos) = response
                .interact_pointer_pos()
                .and_then(|pos| viewport.screen_to_sensor(image_rect, pos))
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
                    .and_then(|pos| viewport.screen_to_sensor(image_rect, pos)),
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
    } else if workspace.tool == PreviewTool::LineProfile {
        if response.drag_started() {
            if let Some(pointer_pos) = pointer_sensor {
                line_profile_tool.start = Some(pointer_pos);
                line_profile_tool.end = Some(pointer_pos);
                line_profile_tool.recompute(frame);
            }
        }
        if response.dragged() {
            if let Some(start) = line_profile_tool.start {
                if let Some(pointer_pos) = pointer_sensor {
                    line_profile_tool.set_line(start, pointer_pos, frame);
                }
            }
        }
        if response.drag_stopped() {
            if let (Some(start), Some(end)) = (
                line_profile_tool.start,
                pointer_sensor.or(line_profile_tool.end),
            ) {
                line_profile_tool.set_line(start, end, frame);
                line_profile_tool.open_window();
            }
        }
    } else if workspace.tool == PreviewTool::Ruler {
        if response.drag_started() {
            if let Some(pointer_pos) = pointer_sensor {
                ruler_tool.start = Some(pointer_pos);
                ruler_tool.end = Some(pointer_pos);
            }
        }
        if response.dragged() {
            if let Some(start) = ruler_tool.start {
                if let Some(pointer_pos) = pointer_sensor {
                    ruler_tool.set_line(start, pointer_pos);
                }
            }
        }
        if response.drag_stopped() {
            if let (Some(start), Some(pointer_pos)) = (ruler_tool.start, pointer_sensor) {
                ruler_tool.set_line(start, pointer_pos);
            }
        }
    } else if matches!(
        workspace.tool,
        PreviewTool::AnnotateRect | PreviewTool::AnnotateEllipse
    ) {
        let kind = if workspace.tool == PreviewTool::AnnotateRect {
            AnnotationShapeKind::Rectangle
        } else {
            AnnotationShapeKind::Ellipse
        };
        if response.drag_started() {
            if let Some(pointer_pos) = pointer_sensor {
                annotation_manager.start_drawing(kind, pointer_pos);
            }
        }
        if response.dragged() {
            if let Some(pointer_pos) = pointer_sensor {
                annotation_manager.update_drawing(pointer_pos);
            }
        }
        if response.drag_stopped() {
            annotation_manager.finish_drawing();
        }
    } else if response.dragged() && workspace.zoom > PREVIEW_ZOOM_MIN {
        let delta = ui.ctx().input(|input| input.pointer.delta());
        workspace.pan += egui::vec2(
            -delta.x * viewport.visible_sensor_rect.width() / image_rect.width().max(1.0),
            -delta.y * viewport.visible_sensor_rect.height() / image_rect.height().max(1.0),
        );
    } else if response.clicked() {
        if let Some(pointer_pos) = pointer_sensor {
            annotation_manager.select_at(pointer_pos);
        }
    }

    let painter = ui
        .painter()
        .with_clip_rect(image_rect.intersect(ui.clip_rect()));
    if let Some(current_roi) = viewport.roi_rect {
        if !workspace.crop_to_roi {
            paint_sensor_rect(
                &painter,
                image_rect,
                viewport,
                current_roi,
                egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 196, 64)),
            );
        }
    }
    if let Some(pending_roi) = workspace.pending_roi {
        paint_sensor_rect(
            &painter,
            image_rect,
            viewport,
            pending_roi,
            egui::Stroke::new(2.0, egui::Color32::WHITE),
        );
    }
    if let (Some(start), Some(end)) = (line_profile_tool.start, line_profile_tool.end) {
        paint_sensor_line_with_shadow(
            &painter,
            image_rect,
            viewport,
            start,
            end,
            egui::Color32::YELLOW,
            2.0,
        );
    }
    if let (Some(start), Some(end), Some(measurement)) = (
        ruler_tool.start,
        ruler_tool.end,
        ruler_tool.measurement(nm_per_pixel),
    ) {
        paint_sensor_line_with_shadow(
            &painter,
            image_rect,
            viewport,
            start,
            end,
            egui::Color32::from_rgb(64, 220, 255),
            2.0,
        );
        let midpoint = viewport.sensor_to_screen(
            image_rect,
            egui::pos2(measurement.midpoint.0, measurement.midpoint.1),
        );
        paint_outlined_text(
            &painter,
            midpoint,
            egui::Align2::CENTER_BOTTOM,
            &format!(
                "{:.1} px | {:.2} µm",
                measurement.pixel_distance, measurement.micrometers
            ),
            egui::FontId::proportional(13.0),
            egui::Color32::WHITE,
        );
    }
    paint_annotations(&painter, image_rect, viewport, annotation_manager);
    if let Some(shape) = annotation_manager.pending_shape() {
        paint_annotation_shape(
            &painter,
            image_rect,
            viewport,
            &shape,
            egui::Color32::WHITE,
            false,
        );
    }
    if scale_bar_settings.show {
        paint_scale_bar(
            &painter,
            image_rect,
            viewport,
            scale_bar_settings,
            nm_per_pixel,
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

fn paint_sensor_line(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    start: (u16, u16),
    end: (u16, u16),
    stroke: egui::Stroke,
) {
    painter.line_segment(
        [
            viewport.sensor_to_screen(
                image_rect,
                egui::pos2(f32::from(start.0), f32::from(start.1)),
            ),
            viewport.sensor_to_screen(image_rect, egui::pos2(f32::from(end.0), f32::from(end.1))),
        ],
        stroke,
    );
}

fn paint_sensor_line_with_shadow(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    start: (u16, u16),
    end: (u16, u16),
    color: egui::Color32,
    width: f32,
) {
    paint_sensor_line(
        painter,
        image_rect,
        viewport,
        start,
        end,
        egui::Stroke::new(
            width + 2.0,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 160),
        ),
    );
    paint_sensor_line(
        painter,
        image_rect,
        viewport,
        start,
        end,
        egui::Stroke::new(width, color),
    );
}

fn paint_outlined_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) {
    let outline = egui::Color32::from_rgba_premultiplied(0, 0, 0, 200);
    for offset in [
        egui::vec2(-1.0, -1.0),
        egui::vec2(1.0, -1.0),
        egui::vec2(-1.0, 1.0),
        egui::vec2(1.0, 1.0),
    ] {
        painter.text(pos + offset, anchor, text, font.clone(), outline);
    }
    painter.text(pos, anchor, text, font, color);
}

fn paint_annotations(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    annotation_manager: &AnnotationManager,
) {
    for annotation in annotation_manager.annotations() {
        let selected = annotation_manager.selected_id() == Some(annotation.id);
        paint_annotation_shape(
            painter,
            image_rect,
            viewport,
            &annotation.shape,
            annotation.color,
            selected,
        );
    }
}

fn paint_annotation_shape(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    shape: &AnnotationShape,
    color: egui::Color32,
    selected: bool,
) {
    let stroke = egui::Stroke::new(if selected { 2.5 } else { 1.5 }, color);
    match shape {
        AnnotationShape::Rectangle { min, max } => {
            let rect = egui::Rect::from_min_max(
                viewport
                    .sensor_to_screen(image_rect, egui::pos2(f32::from(min.0), f32::from(min.1))),
                viewport
                    .sensor_to_screen(image_rect, egui::pos2(f32::from(max.0), f32::from(max.1))),
            );
            painter.rect_stroke(rect, 0.0, stroke);
        }
        AnnotationShape::Ellipse {
            center,
            radius_x,
            radius_y,
        } => {
            let screen_center = viewport.sensor_to_screen(
                image_rect,
                egui::pos2(f32::from(center.0), f32::from(center.1)),
            );
            let radius_screen = viewport.sensor_to_screen(
                image_rect,
                egui::pos2(
                    f32::from(center.0.saturating_add(*radius_x)),
                    f32::from(center.1.saturating_add(*radius_y)),
                ),
            );
            let radii = egui::vec2(
                (radius_screen.x - screen_center.x).abs(),
                (radius_screen.y - screen_center.y).abs(),
            );
            let points: Vec<egui::Pos2> = (0..=48)
                .map(|step| {
                    let angle = std::f32::consts::TAU * step as f32 / 48.0;
                    egui::pos2(
                        screen_center.x + radii.x * angle.cos(),
                        screen_center.y + radii.y * angle.sin(),
                    )
                })
                .collect();
            painter.add(egui::Shape::line(points, stroke));
        }
    }
}

fn paint_scale_bar(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    settings: &ScaleBarSettings,
    nm_per_pixel: f64,
) {
    let pixels_per_sensor_pixel =
        image_rect.width() / viewport.visible_sensor_rect.width().max(1.0);
    let Some(spec) = compute_scale_bar(nm_per_pixel, pixels_per_sensor_pixel) else {
        return;
    };

    let margin = 18.0;
    let bar_height = 6.0;
    let y = match settings.position {
        ScaleBarPosition::TopLeft | ScaleBarPosition::TopRight => image_rect.top() + margin,
        ScaleBarPosition::BottomLeft | ScaleBarPosition::BottomRight => {
            image_rect.bottom() - margin
        }
    };
    let x = match settings.position {
        ScaleBarPosition::TopLeft | ScaleBarPosition::BottomLeft => image_rect.left() + margin,
        ScaleBarPosition::TopRight | ScaleBarPosition::BottomRight => {
            image_rect.right() - margin - spec.screen_width
        }
    };
    let rect =
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(spec.screen_width, bar_height));
    painter.rect_filled(rect, 1.5, settings.color);
    paint_outlined_text(
        painter,
        egui::pos2(rect.center().x, rect.top() - 4.0),
        egui::Align2::CENTER_BOTTOM,
        &spec.label,
        egui::FontId::proportional(13.0),
        settings.color,
    );
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
    config.global.sensor_width = info.width;
    config.global.sensor_height = info.height;
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
