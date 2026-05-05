use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Serialize};

use augur_core::{
    analysis::{AnalysisOutput, AnalysisSeverity, AnalysisWarning, Overlay},
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
    FfiCdEvent, GlobalSettings, HostActionRequest, HostActionRequestQueue, HostActionScope,
    HostActionScopePayload, HostDatasetKind, HostViewKind, Image2dV1, PluginInput, Series1dV1,
    SettingsSchema, TableColumnValues, TableDatasetV1, CTX_GLOBAL_SETTINGS,
    CTX_INVESTIGATION_ACTION_REQUESTS, HOST_ACTION_CLUSTER_ROWS_PARAM,
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
        render_density2d_view, render_density_window_viewport, render_image2d_view,
        render_image_window_viewport, render_line_series_view, render_linked_table_view,
        render_scatter2d_view, render_scatter_window_viewport, render_series_window_viewport,
        render_summary_card, render_table_window_viewport, reset_provider_for_dataset,
        resolve_host_view_registry, DensityWindowViewportData, HostDatasetSnapshot,
        HostRegistryContribution, HostViewImageFormat, HostViewProviderKey, HostViewRenderState,
        HostViewUiActions, ImageWindowViewportData, LinkedTableViewOptions, ResolvedHostView,
        ResolvedHostViewRegistry, Scatter2dViewOptions, ScatterWindowViewportData,
        SeriesWindowViewportData, SummaryCardOptions, TableCellFormatOptions,
        TableWindowViewportData,
    },
    hotpixel::BuiltInHotpixelDetection,
    inspection_3d::{
        draw_investigation_3d, Investigation3dFocusVolume, Investigation3dLayer,
        Investigation3dPoint, Investigation3dRenderer, Investigation3dScene,
    },
    investigation::{
        coordinate_2d_for_row, coordinate_3d_for_row, dataset_layer_id, filtered_row_indices,
        retain_rows_in_frame_span, row_key_for_row, stable_row_id_value, AnalysisRoi,
        Investigation2dPoint, InvestigationLayout, StableRowKey,
    },
    plugin_loader::{PluginEventHistory, PluginManager},
    plugin_settings_ui::render_plugin_settings,
    preview::{
        cached_frame_histogram, compute_auto_contrast_max, compute_frame_histogram,
        reset_preview_render_cache, PreviewDisplaySettings, PreviewMode,
    },
    preview_perf::{PerfMetricSnapshot, PreviewPerfStats},
    preview_renderer::{PreviewDisplayTexture, PreviewRenderRequest, PreviewRenderer},
    render_backend::ActiveRendererInfo,
    settings::draw_settings,
    viewer_widget::{
        draw_replay_transport, draw_text_placeholder, draw_viewer, replay_speed_matches, AppMode,
        PreviewHistogramRequest, PreviewTool, ViewerAuxChanges, ViewerInput, ViewerOutput,
        ViewerReplayState, ViewerState, REPLAY_SPEED_OPTIONS,
    },
};

const COLLAPSED_PANEL_WIDTH: f32 = 22.0;
const EVENT_STORE_MEBIBYTE: usize = 1024 * 1024;
pub(crate) const PANEL_ROUNDING: f32 = 6.0;
const UI_THEME_STORAGE_KEY: &str = "augur_gui.theme_preference";
const REPLAY_FRAME_HISTORY_CAPACITY: usize = 8;
const RAW_EVENTS_ON_LAYER_ID: &str = "host.raw_events.on";
const RAW_EVENTS_OFF_LAYER_ID: &str = "host.raw_events.off";
const RAW_EVENTS_ON_COLOR: [u8; 4] = [255, 186, 92, 240];
const RAW_EVENTS_OFF_COLOR: [u8; 4] = [92, 214, 201, 220];

type CachedHostDataset = Result<Option<HostDatasetSnapshot>, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum UiThemePreference {
    Dark,
    Light,
}

impl UiThemePreference {
    fn from_dark_mode(dark_mode: bool) -> Self {
        if dark_mode {
            Self::Dark
        } else {
            Self::Light
        }
    }

    fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    fn visuals(self) -> egui::Visuals {
        match self {
            Self::Dark => egui::Visuals::dark(),
            Self::Light => egui::Visuals::light(),
        }
    }
}

fn process_interval_for_layout(
    layout: InvestigationLayout,
    preview_interval_ms: u64,
    point_cloud_interval_ms: u64,
) -> Duration {
    match layout {
        InvestigationLayout::Preview2dOnly => Duration::from_millis(preview_interval_ms),
        InvestigationLayout::Split2d3d => {
            Duration::from_millis(preview_interval_ms.min(point_cloud_interval_ms))
        }
        InvestigationLayout::Inspection3dOnly => Duration::from_millis(point_cloud_interval_ms),
    }
}

fn viewport_stream_active(mode: AppMode, replay: ViewerReplayState) -> bool {
    matches!(mode, AppMode::Previewing | AppMode::Recording)
        || (mode == AppMode::Replaying
            && replay.active
            && ((!replay.paused && !replay.finished) || replay.stepping))
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

fn draw_preview_perf_row(
    ui: &mut egui::Ui,
    label: &str,
    tooltip: &str,
    metric: PerfMetricSnapshot,
) {
    ui.label(label).on_hover_text(tooltip);
    if metric.samples == 0 {
        ui.weak("-");
        ui.weak("-");
        ui.weak("-");
    } else {
        ui.monospace(format!("{:.2}", metric.last_ms));
        ui.monospace(format!("{:.2}", metric.avg_ms));
        ui.monospace(format!("{:.2}", metric.max_ms));
    }
    ui.end_row();
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

fn raw_event_depth_scale(sensor_height: u16, time_window_ms: f32) -> f32 {
    f32::from(sensor_height.max(1)) / time_window_ms.max(1.0)
}

fn sensor_y_to_world(sensor_height: u16, sensor_y: f64) -> f32 {
    (f64::from(sensor_height.saturating_sub(1)) - sensor_y).max(0.0) as f32
}

fn raw_event_point_position(
    event: CdEvent,
    latest_timestamp: u64,
    sensor_height: u16,
    effective_time_window_ms: f32,
) -> [f32; 3] {
    let age_ms = latest_timestamp.saturating_sub(event.timestamp) as f32 / 1_000.0;
    [
        f32::from(event.x),
        sensor_y_to_world(sensor_height, f64::from(event.y)),
        -age_ms * raw_event_depth_scale(sensor_height, effective_time_window_ms),
    ]
}

fn roi_is_effectively_full_frame(roi: &AnalysisRoi, sensor_width: u16, sensor_height: u16) -> bool {
    roi.x_min <= 0.0
        && roi.y_min <= 0.0
        && roi.x_max >= f64::from(sensor_width.saturating_sub(1))
        && roi.y_max >= f64::from(sensor_height.saturating_sub(1))
}

fn raw_event_focus_volume(
    roi: &AnalysisRoi,
    earliest_timestamp: u64,
    latest_timestamp: u64,
    sensor_height: u16,
    effective_time_window_ms: f32,
) -> Investigation3dFocusVolume {
    let z_min = -(latest_timestamp.saturating_sub(earliest_timestamp) as f32) / 1_000.0
        * raw_event_depth_scale(sensor_height, effective_time_window_ms);
    Investigation3dFocusVolume {
        min: [
            roi.x_min as f32,
            sensor_y_to_world(sensor_height, roi.y_max),
            z_min,
        ],
        max: [
            roi.x_max as f32,
            sensor_y_to_world(sensor_height, roi.y_min),
            0.0,
        ],
        label: format!(
            "x {:.0}..{:.0}, y {:.0}..{:.0}",
            roi.x_min, roi.x_max, roi.y_min, roi.y_max
        ),
        color: [255, 221, 116, 220],
    }
}

fn derived_replay_preview_interval_ms(acq_time_ms: u64, speed: f32) -> u64 {
    if speed.is_finite() && speed > 0.0 {
        (acq_time_ms as f64 / speed as f64).round() as u64
    } else {
        10
    }
    .clamp(10, 200)
}

fn acq_time_us_from_ms(acq_time_ms: u64) -> u64 {
    acq_time_ms.max(1).saturating_mul(1_000)
}

fn sync_acq_time_atomic(acq_time_us: &Arc<AtomicU64>, acq_time_ms: u64) {
    acq_time_us.store(acq_time_us_from_ms(acq_time_ms), Ordering::Relaxed);
}

fn replay_time_from_fraction(fraction: f32, total_duration_us: u64) -> u64 {
    ((total_duration_us as f64 * fraction.clamp(0.0, 1.0) as f64).round() as u64)
        .min(total_duration_us)
}

fn replay_fraction_from_time(time_us: u64, total_duration_us: u64) -> f32 {
    if total_duration_us == 0 {
        0.0
    } else {
        (time_us as f64 / total_duration_us as f64) as f32
    }
    .clamp(0.0, 1.0)
}

fn replay_time_from_position_sources(
    finished: bool,
    pending_fraction: Option<f32>,
    displayed_window_end_us: Option<u64>,
    current_timestamp_us: u64,
    first_timestamp_us: u64,
    total_duration_us: u64,
    byte_fraction: f32,
) -> u64 {
    if finished {
        return total_duration_us;
    }
    if let Some(fraction) = pending_fraction {
        return replay_time_from_fraction(fraction, total_duration_us);
    }
    if let Some(window_end_us) = displayed_window_end_us {
        return window_end_us
            .saturating_sub(first_timestamp_us)
            .min(total_duration_us);
    }
    if current_timestamp_us > 0 {
        return current_timestamp_us
            .saturating_sub(first_timestamp_us)
            .min(total_duration_us);
    }
    replay_time_from_fraction(byte_fraction, total_duration_us)
}

fn replay_seek_target_reached(target_timestamp_us: Option<u64>, frame_window_end_us: u64) -> bool {
    target_timestamp_us.is_none_or(|target_timestamp_us| frame_window_end_us >= target_timestamp_us)
}

fn replay_step_target_time_us(
    current_time_us: u64,
    frame_step_us: u64,
    frame_steps: i64,
    total_duration_us: u64,
) -> u64 {
    if total_duration_us == 0 {
        return 0;
    }

    let step_us = frame_step_us.max(1) as i128;
    (current_time_us as i128 + frame_steps as i128 * step_us).clamp(0, total_duration_us as i128)
        as u64
}

fn replay_step_uses_current_controller(
    frame_steps: i64,
    replay_paused: bool,
    replay_finished: bool,
    controller_active: bool,
) -> bool {
    frame_steps > 0 && replay_paused && !replay_finished && controller_active
}

fn replay_history_has_display_override(history_len: usize, cursor: Option<usize>) -> bool {
    matches!(cursor, Some(cursor) if history_len > 0 && cursor + 1 < history_len)
}

fn replay_history_step_target(
    history_len: usize,
    cursor: Option<usize>,
    frame_steps: i64,
) -> Option<usize> {
    if frame_steps == 0 || history_len == 0 {
        return None;
    }

    let cursor = cursor?;
    let target = cursor as i64 + frame_steps;
    (0..history_len as i64)
        .contains(&target)
        .then_some(target as usize)
}

fn pipeline_stream_active(
    mode: AppMode,
    replay_paused: bool,
    replay_finished: bool,
    replay_pause_after_seek_frame: bool,
) -> bool {
    matches!(mode, AppMode::Previewing | AppMode::Recording)
        || (mode == AppMode::Replaying && !replay_paused && !replay_finished)
        || (mode == AppMode::Replaying && replay_pause_after_seek_frame)
}

#[derive(Debug, Clone)]
struct HostDatasetCacheEntry {
    generation: u64,
    snapshot: CachedHostDataset,
}

#[derive(Debug, Clone)]
struct ReplayFrameSnapshot {
    frame: PreviewFrame,
    analysis_output: AnalysisOutput,
    bytes_read: u64,
}

fn replay_snapshot_frame(frame: &PreviewFrame) -> PreviewFrame {
    let mut snapshot = frame.clone();
    if snapshot.event_source.is_some() && snapshot.event_range.is_some() {
        snapshot.events = None;
    }
    snapshot
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
    investigation_renderer: Investigation3dRenderer,
    investigation_points_2d: Vec<Investigation2dPoint>,
    investigation_scene_3d: Investigation3dScene,
    texture: Option<PreviewDisplayTexture>,
    frame: Option<PreviewFrame>,
    time_surface_hover_value: Option<u8>,
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
            investigation_renderer: Investigation3dRenderer::Disabled,
            investigation_points_2d: Vec::new(),
            investigation_scene_3d: Investigation3dScene::default(),
            texture: None,
            frame: None,
            time_surface_hover_value: None,
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
    texture: Option<PreviewDisplayTexture>,
    preview_renderer: PreviewRenderer,
    investigation_renderer: Investigation3dRenderer,
    preview_perf: PreviewPerfStats,
    renderer_info: ActiveRendererInfo,
    preview_renderer_notice: Option<String>,
    latest_frame: Option<PreviewFrame>,
    last_preview_process_at: Option<Instant>,
    replay_controls: Option<ReplayControls>,
    replay_file_info: Option<ReplayFileInfo>,
    replay_decoded_events: Option<Arc<Vec<CdEvent>>>,
    replay_paused: bool,
    replay_finished: bool,
    replay_pause_after_seek_frame: bool,
    replay_pause_after_seek_target_timestamp_us: Option<u64>,
    replay_pending_fraction: Option<f32>,
    replay_frame_history: VecDeque<ReplayFrameSnapshot>,
    replay_history_cursor: Option<usize>,
    replay_speed: f32,
    replay_notice: Option<String>,
    replay_open_task: Option<ReplayOpenTask>,
    saved_live_state: Option<SavedLiveState>,
    hotpixel_detection: BuiltInHotpixelDetection,
    plugin_manager: PluginManager,
    plugin_context_data: HashMap<String, Vec<u8>>,
    persistent_context_data: HashMap<String, Vec<u8>>,
    event_store: PluginEventHistory,
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
    theme_preference: UiThemePreference,
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
    pending_action_requests: Vec<HostActionRequest>,
    next_action_request_id: u64,
    action_modal: Option<ActionModalState>,
}

#[derive(Debug, Clone)]
struct ActionModalState {
    action_id: String,
    title: String,
    scope_payload: HostActionScopePayload,
    schema: Option<SettingsSchema>,
    params: serde_json::Value,
}

impl CameraApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        let theme_preference = cc
            .storage
            .and_then(|storage| storage.get_string(UI_THEME_STORAGE_KEY))
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_else(|| {
                UiThemePreference::from_dark_mode(cc.egui_ctx.style().visuals.dark_mode)
            });
        cc.egui_ctx.set_visuals(theme_preference.visuals());

        let mut plugin_manager = PluginManager::new_default();
        let plugin_scan_error = plugin_manager.scan_and_load().err();
        let global_defaults = GlobalSettingsConfig::default();
        let renderer_info = ActiveRendererInfo::from_creation_context(cc);
        let (preview_renderer, preview_renderer_notice) = PreviewRenderer::new(cc);
        let investigation_renderer = Investigation3dRenderer::new(cc);
        eprintln!(
            "Augur renderer requested={} active={} backend={} adapter={} preview={}",
            renderer_info.requested.label(),
            renderer_info.active_renderer,
            renderer_info.backend,
            renderer_info.adapter,
            preview_renderer.label(),
        );

        let mut app = Self {
            config: CameraConfig::default(),
            output_path: format!("./output_{}.raw", format_timestamp_now()),
            always_timestamp: false,
            replay_path: None,
            mode: AppMode::Idle,
            controller: None,
            texture: None,
            preview_renderer,
            investigation_renderer,
            preview_perf: PreviewPerfStats::default(),
            renderer_info,
            preview_renderer_notice,
            latest_frame: None,
            last_preview_process_at: None,
            replay_controls: None,
            replay_file_info: None,
            replay_decoded_events: None,
            replay_paused: false,
            replay_finished: false,
            replay_pause_after_seek_frame: false,
            replay_pause_after_seek_target_timestamp_us: None,
            replay_pending_fraction: None,
            replay_frame_history: VecDeque::with_capacity(REPLAY_FRAME_HISTORY_CAPACITY),
            replay_history_cursor: None,
            replay_speed: 1.0,
            replay_notice: None,
            replay_open_task: None,
            saved_live_state: None,
            hotpixel_detection: BuiltInHotpixelDetection::default(),
            plugin_manager,
            plugin_context_data: HashMap::new(),
            persistent_context_data: HashMap::new(),
            event_store: PluginEventHistory::default(),
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
            theme_preference,
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
            pending_action_requests: Vec::new(),
            next_action_request_id: 1,
            action_modal: None,
        };
        app.event_store
            .set_memory_budget(mib_to_bytes(global_defaults.event_store_budget_mib));
        if !app.investigation_renderer.is_wgpu() {
            app.viewer.investigation.layout = InvestigationLayout::Preview2dOnly;
        }
        app.sync_config_global_from_runtime();
        app.refresh_host_view_registry();
        app
    }

    fn event_store_budget_mib(&self) -> u64 {
        (self.event_store.memory_budget_bytes() / EVENT_STORE_MEBIBYTE).max(1) as u64
    }

    fn apply_theme_to_ctx(&self, ctx: &egui::Context) {
        ctx.set_visuals(self.theme_preference.visuals());
    }

    fn set_theme_preference(&mut self, ctx: &egui::Context, theme_preference: UiThemePreference) {
        if self.theme_preference != theme_preference {
            self.theme_preference = theme_preference;
            self.apply_theme_to_ctx(ctx);
            ctx.request_repaint();
        }
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

    fn active_investigation_layout(&self) -> InvestigationLayout {
        self.with_active_viewer(|viewer| viewer.investigation.layout)
    }

    fn mode_label(&self) -> &'static str {
        match self.mode {
            AppMode::Idle => "Idle",
            AppMode::Previewing => "Previewing",
            AppMode::Recording => "Recording",
            AppMode::Replaying => "Replaying",
        }
    }

    fn set_active_investigation_layout(&mut self, layout: InvestigationLayout) {
        let layout = if self.investigation_renderer.is_wgpu() {
            layout
        } else {
            InvestigationLayout::Preview2dOnly
        };
        self.with_active_viewer_mut(|viewer| {
            viewer.investigation.layout = layout;
            viewer.workspace.clear_selection();
        });
    }

    fn sync_active_analysis_roi(&mut self) {
        let fallback_roi = AnalysisRoi::from_sensor_roi(self.config.roi);
        self.with_active_viewer_mut(|viewer| {
            if !viewer.investigation.link_roi_between_2d_and_3d {
                viewer.investigation.active_analysis_roi = None;
                return;
            }

            viewer.investigation.active_analysis_roi =
                viewer.annotation_manager.selected_annotation().map_or_else(
                    || Some(fallback_roi.clone()),
                    |annotation| {
                        let bounds = annotation.shape.bounds_rect();
                        Some(AnalysisRoi {
                            x_min: f64::from(bounds.min.0),
                            x_max: f64::from(bounds.max.0),
                            y_min: f64::from(bounds.min.1),
                            y_max: f64::from(bounds.max.1),
                        })
                    },
                );
        });
    }

    fn render_investigation_actions(&mut self, ui: &mut egui::Ui) {
        let actions: Vec<(String, String, HostActionScope, Option<serde_json::Value>)> = self
            .host_view_registry
            .actions()
            .map(|action| {
                (
                    action.descriptor.id.clone(),
                    action.descriptor.title.clone(),
                    action.descriptor.scope.clone(),
                    action.descriptor.param_schema.clone(),
                )
            })
            .collect();
        if actions.is_empty() {
            return;
        }
        ui.collapsing("Actions", |ui| {
            ui.small(
                "Row- and cluster-scoped actions published by plugins. Apply to queue; the plugin consumes on the next frame.",
            );
            for (id, title, scope, param_schema) in actions {
                let payload = self.resolve_action_scope_payload(&scope);
                let enabled = payload.is_some();
                let hover = match &scope {
                    HostActionScope::Dataset { dataset_id } => {
                        format!("Dataset-wide action on {dataset_id}.")
                    }
                    HostActionScope::Row { dataset_id } => format!(
                        "Select exactly one row on {dataset_id} to enable.",
                    ),
                    HostActionScope::Cluster {
                        dataset_id,
                        group_column,
                    } => format!(
                        "Select rows on {dataset_id} sharing the same {group_column} value.",
                    ),
                };
                ui.horizontal(|ui| {
                    let response = ui.add_enabled(enabled, egui::Button::new(&title));
                    let response = response.on_hover_text(&hover);
                    if response.clicked() {
                        if let Some(payload) = payload {
                            let schema = param_schema
                                .as_ref()
                                .and_then(|v| serde_json::from_value::<SettingsSchema>(v.clone()).ok());
                            self.open_action_modal(&id, &title, payload, schema);
                        }
                    }
                });
            }
        });
    }

    fn render_investigation_inspector(&mut self, ui: &mut egui::Ui) {
        let current_layout = self.active_investigation_layout();
        let (
            mut link_roi_between_2d_and_3d,
            active_roi,
            selected_row,
            hovered_row,
            focused_layers,
            mut layer_ids,
        ) = self.with_active_viewer(|viewer| {
            (
                viewer.investigation.link_roi_between_2d_and_3d,
                viewer.investigation.active_analysis_roi.clone(),
                viewer.investigation.primary_selection().cloned(),
                viewer.investigation.hovered_row.clone(),
                viewer.investigation.focused_layers.clone(),
                viewer
                    .investigation
                    .layer_styles
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        });
        layer_ids.sort();
        let (raw_layer_ids, analysis_layer_ids): (Vec<_>, Vec<_>) =
            layer_ids.into_iter().partition(|layer_id| {
                matches!(
                    layer_id.as_str(),
                    RAW_EVENTS_ON_LAYER_ID | RAW_EVENTS_OFF_LAYER_ID
                )
            });

        ui.collapsing("Workspace", |ui| {
            ui.small(
                "Use the top bar to switch between 2D, split, and 3D. This panel keeps the linked ROI, selection, and layer styling together.",
            );
            ui.small(format!("Current layout: {}", current_layout.label()));
            if ui
                .checkbox(
                    &mut link_roi_between_2d_and_3d,
                    "Link 2D ROI into the 3D focus volume",
                )
                .on_hover_text(
                    "Use the selected 2D ROI to drive linked table filtering and the highlighted 3D focus box.",
                )
                .changed()
            {
                self.with_active_viewer_mut(|viewer| {
                    viewer.investigation.link_roi_between_2d_and_3d = link_roi_between_2d_and_3d;
                });
                self.sync_active_analysis_roi();
            }

            match active_roi {
                Some(roi) => {
                    ui.small(format!(
                        "Linked ROI: x {:.1}..{:.1}, y {:.1}..{:.1}",
                        roi.x_min, roi.x_max, roi.y_min, roi.y_max
                    ));
                }
                None => {
                    ui.small("Linked ROI: off");
                }
            }

            if let Some(selected) = &selected_row {
                let layer_title = self.with_active_viewer(|viewer| {
                    viewer
                        .investigation
                        .layer_styles
                        .iter()
                        .find(|(id, _)| selected.dataset_id.contains(id.as_str()))
                        .map(|(_, s)| s.title.clone())
                });
                let label = layer_title.unwrap_or_else(|| selected.dataset_id.clone());
                ui.label(format!("Selected item: {label} row {}", selected.row_id));
            } else {
                ui.small("Selected item: none");
            }

            if let Some(hovered) = &hovered_row {
                ui.small(format!("Hovered row: {}", hovered.row_id));
            }

            if (selected_row.is_some() || hovered_row.is_some())
                && ui
                    .small_button("Clear selection")
                    .on_hover_text("Clear the current linked 2D/3D/table selection.")
                    .clicked()
            {
                self.with_active_viewer_mut(|viewer| {
                    viewer.investigation.clear_selection();
                    viewer.investigation.hovered_row = None;
                });
            }
            ui.small("Shortcuts: 1/2/3 layout, L link ROI, Esc clear selection, F focus 3D");
        });

        ui.separator();
        self.render_investigation_actions(ui);

        ui.separator();
        ui.collapsing("Layers", |ui| {
            ui.small(
                "Raw event ON/OFF layers style the current point cloud. Extra analysis layers only affect the 3D scene when a plugin publishes a 3D scatter view.",
            );
            if !focused_layers.is_empty() {
                ui.small(format!(
                    "Isolated: {}",
                    focused_layers
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if ui.button("Show all layers").clicked() {
                self.with_active_viewer_mut(|viewer| {
                    viewer.investigation.focused_layers.clear();
                    let layer_ids: Vec<String> =
                        viewer.investigation.layer_styles.keys().cloned().collect();
                    for layer_id in layer_ids {
                        viewer.investigation.set_layer_visible(layer_id, true);
                    }
                });
            }
            ui.separator();
            if !raw_layer_ids.is_empty() {
                ui.strong("Point Cloud");
                self.render_investigation_layer_cards(ui, raw_layer_ids.as_slice());
            }
            if !analysis_layer_ids.is_empty() {
                if !raw_layer_ids.is_empty() {
                    ui.separator();
                }
                ui.strong("Analysis Layers");
                self.render_investigation_layer_cards(ui, analysis_layer_ids.as_slice());
            } else {
                ui.small("No additional analysis layers are active.");
            }

            ui.separator();
            ui.small(
                "Raw-event history controls now live in the 3D toolbar so the time span can be widened while you inspect the cloud.",
            );
        });

        ui.separator();
        ui.collapsing("Status & warnings", |ui| {
            ui.small(format!("Mode: {}", self.mode_label()));
            if self.config_dirty || self.acq_dirty {
                let mut dirty = Vec::new();
                if self.config_dirty {
                    dirty.push("settings");
                }
                if self.acq_dirty {
                    dirty.push("acquisition timing");
                }
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Apply pending changes: {}", dirty.join(", ")),
                );
            } else {
                ui.small("Host settings and current outputs are in sync.");
            }

            if self.analysis_output.warnings.is_empty() {
                ui.small("No analysis warnings.");
            } else {
                for warning in self.analysis_output.warnings.iter().take(6) {
                    let color = match warning.severity {
                        augur_core::analysis::AnalysisSeverity::Info => {
                            ui.visuals().weak_text_color()
                        }
                        augur_core::analysis::AnalysisSeverity::Warning => {
                            ui.visuals().warn_fg_color
                        }
                        augur_core::analysis::AnalysisSeverity::Error => {
                            ui.visuals().error_fg_color
                        }
                    };
                    ui.colored_label(color, format!("{}: {}", warning.source, warning.message));
                }
                let remaining = self.analysis_output.warnings.len().saturating_sub(6);
                if remaining > 0 {
                    ui.small(format!("{remaining} more warning(s) remain."));
                }
            }

            if let Some(notice) = &self.analysis_notice {
                ui.label(format!("Notice: {notice}"));
            }
            if let Some(error) = &self.last_error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    fn render_investigation_layer_cards(&mut self, ui: &mut egui::Ui, layer_ids: &[String]) {
        for layer_id in layer_ids {
            let (mut style, mut visible) = self.with_active_viewer(|viewer| {
                let style = viewer
                    .investigation
                    .layer_styles
                    .get(layer_id)
                    .cloned()
                    .expect("layer id came from layer_styles");
                let visible = viewer.investigation.layer_visible(layer_id, style.visible);
                (style, visible)
            });
            let mut changed = false;

            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let swatch = egui::Color32::from_rgba_unmultiplied(
                        style.color[0],
                        style.color[1],
                        style.color[2],
                        style.color[3],
                    );
                    ui.colored_label(swatch, "■");
                    changed |= ui
                        .checkbox(&mut visible, &style.title)
                        .on_hover_text("Show or hide this layer in linked investigation views.")
                        .changed();
                    if ui
                        .small_button("Only")
                        .on_hover_text("Hide all other layers so this one is easier to inspect.")
                        .clicked()
                    {
                        self.with_active_viewer_mut(|viewer| {
                            viewer.investigation.isolate_layer(layer_id);
                        });
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Color")
                        .on_hover_text("Tint used for this layer in 2D and 3D.");
                    changed |= ui
                        .color_edit_button_srgba_unmultiplied(&mut style.color)
                        .on_hover_text("Tint used for this layer in 2D and 3D.")
                        .changed();

                    let is_raw_event_layer = matches!(
                        layer_id.as_str(),
                        RAW_EVENTS_ON_LAYER_ID | RAW_EVENTS_OFF_LAYER_ID
                    );
                    ui.label("Shape").on_hover_text(
                        "Marker shape affects linked 2D markers. The current 3D point cloud uses round billboards.",
                    );
                    if is_raw_event_layer {
                        ui.small("3D round sprites only").on_hover_text(
                            "Raw-event point clouds currently support color and size, but not alternate 3D marker shapes.",
                        );
                    } else {
                        egui::ComboBox::from_id_source(("investigation_layer_shape", layer_id))
                            .selected_text(host_marker_shape_label(style.marker_shape))
                            .show_ui(ui, |ui| {
                                for shape in [
                                    augur_plugin_api::HostMarkerShape::Point,
                                    augur_plugin_api::HostMarkerShape::Cross,
                                    augur_plugin_api::HostMarkerShape::Box,
                                    augur_plugin_api::HostMarkerShape::Ellipse,
                                    augur_plugin_api::HostMarkerShape::Diamond,
                                    augur_plugin_api::HostMarkerShape::FilledCircle,
                                ] {
                                    ui.selectable_value(
                                        &mut style.marker_shape,
                                        shape,
                                        host_marker_shape_label(shape),
                                    );
                                }
                            });
                    }

                    ui.label("Size").on_hover_text(
                        "Base marker size. The 3D view also uses this for the point-cloud sprite radius.",
                    );
                    changed |= ui
                        .add(egui::Slider::new(&mut style.size, 1.0..=12.0))
                        .on_hover_text(
                            "Base marker size. The 3D view also uses this for the point-cloud sprite radius.",
                        )
                        .changed();
                });
            });

            if changed {
                self.with_active_viewer_mut(|viewer| {
                    if let Some(entry) = viewer.investigation.layer_styles.get_mut(layer_id) {
                        *entry = style.clone();
                    }
                    viewer
                        .investigation
                        .set_layer_visible(layer_id.clone(), visible);
                    if visible {
                        viewer.investigation.focused_layers.remove(layer_id);
                    }
                });
            }
        }
    }

    fn open_popup_viewer(&mut self) {
        if self.popup_open {
            return;
        }
        let mut data = self.popup_shared.lock().unwrap();
        data.viewer = std::mem::take(&mut self.viewer);
        data.investigation_renderer = std::mem::take(&mut self.investigation_renderer);
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
        self.investigation_renderer = std::mem::take(&mut data.investigation_renderer);
        data.close_requested = false;
        data.output = None;
        self.popup_open = false;
    }

    fn clear_replay_frame_history(&mut self) {
        self.replay_frame_history.clear();
        self.replay_history_cursor = None;
    }

    fn replay_history_cursor(&self) -> Option<usize> {
        self.replay_history_cursor
            .filter(|&cursor| cursor < self.replay_frame_history.len())
    }

    fn replay_has_display_override(&self) -> bool {
        replay_history_has_display_override(
            self.replay_frame_history.len(),
            self.replay_history_cursor(),
        )
    }

    fn replay_controller_bytes_read(&self) -> u64 {
        self.replay_controls
            .as_ref()
            .map(|controls| controls.bytes_read.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn record_replay_frame_snapshot(&mut self, frame: &PreviewFrame) {
        if self.mode != AppMode::Replaying {
            return;
        }

        if self.replay_frame_history.len() == REPLAY_FRAME_HISTORY_CAPACITY {
            self.replay_frame_history.pop_front();
            if let Some(cursor) = self.replay_history_cursor {
                self.replay_history_cursor = cursor.checked_sub(1);
            }
        }

        self.replay_frame_history.push_back(ReplayFrameSnapshot {
            frame: replay_snapshot_frame(frame),
            analysis_output: self.analysis_output.clone(),
            bytes_read: self.replay_controller_bytes_read(),
        });
        self.replay_history_cursor = self.replay_frame_history.len().checked_sub(1);
    }

    fn apply_replay_frame_snapshot(
        &mut self,
        ctx: &egui::Context,
        snapshot: ReplayFrameSnapshot,
        history_cursor: usize,
    ) {
        self.replay_history_cursor = Some(history_cursor);
        self.replay_pause_after_seek_frame = false;
        self.replay_pause_after_seek_target_timestamp_us = None;
        self.replay_pending_fraction = None;
        self.replay_paused = true;
        self.replay_finished = self.controller.is_none()
            && !replay_history_has_display_override(
                self.replay_frame_history.len(),
                self.replay_history_cursor,
            )
            && self
                .replay_file_info
                .as_ref()
                .is_some_and(|info| self.replay_controller_bytes_read() >= info.data_len());
        self.analysis_output = snapshot.analysis_output;
        self.update_preview_histogram_from_frame(&snapshot.frame);

        self.with_active_viewer_mut(|viewer| {
            if viewer.needs_line_profile_refresh() {
                viewer.line_profile_tool.recompute(&snapshot.frame);
            }
            viewer.workspace.point_cloud.clear();
            viewer.workspace.point_cloud.push_frame(&snapshot.frame);
        });

        if !self.external_tool_status().is_streaming()
            && self.active_investigation_layout().shows_2d()
        {
            if let Err(err) = self.render_preview_texture_from_frame(ctx, &snapshot.frame) {
                self.last_error = Some(format!("preview render failed: {err}"));
            }
        }

        self.latest_frame = Some(snapshot.frame);
        self.last_preview_process_at = Some(Instant::now());
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
            stepping: self.replay_pause_after_seek_frame,
            speed: self.replay_speed,
            fraction: self.current_replay_fraction(),
            duration_us,
            time_us: self.current_replay_time_us(),
            bytes_read: self.current_replay_bytes_read(),
            data_len,
        }
    }

    fn preview_time_surface_hover_value(
        &mut self,
        preview_mode: PreviewMode,
        hover_sensor: Option<(u16, u16)>,
        time_surface_tau_us: u64,
        frame: Option<&PreviewFrame>,
    ) -> Option<u8> {
        if preview_mode != PreviewMode::TimeSurface {
            return None;
        }
        let (x, y) = hover_sensor?;
        let frame = frame?;
        let width = usize::from(frame.width.max(1));
        let index = usize::from(y) * width + usize::from(x);
        self.preview_renderer
            .query_time_surface_value(frame, time_surface_tau_us, index)
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
        investigation_points_2d: &[Investigation2dPoint],
        investigation_scene_3d: &Investigation3dScene,
    ) {
        if !self.popup_open {
            return;
        }

        let pipeline_stats = self
            .controller
            .as_ref()
            .map(PipelineController::stats_snapshot);
        let detected_hotpixels = self.latest_detected_hotpixels();
        let latest_frame_for_hover = self.latest_frame.clone();
        let (popup_preview_mode, popup_tau_us, popup_hover_sensor) = {
            let data = self.popup_shared.lock().unwrap();
            (
                data.viewer.preview_mode,
                data.viewer.time_surface_tau_us,
                data.viewer.workspace.hover_sensor,
            )
        };
        let popup_time_surface_hover_value = self.preview_time_surface_hover_value(
            popup_preview_mode,
            popup_hover_sensor,
            popup_tau_us,
            latest_frame_for_hover.as_ref(),
        );
        let mut data = self.popup_shared.lock().unwrap();
        data.investigation_points_2d = investigation_points_2d.to_vec();
        data.investigation_scene_3d = investigation_scene_3d.clone();
        data.texture = self.texture.clone();
        data.frame = self.latest_frame.clone();
        data.time_surface_hover_value = popup_time_surface_hover_value;
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
        let needs_preview_refresh = output.needs_preview_refresh();
        let investigation_select_row = output.investigation_select_row.clone();
        let investigation_hover_row = output.investigation_hover_row.clone();

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
        if let Some(frame_steps) = output.replay_step_frames {
            self.step_replay(ctx, frame_steps);
        }
        if let Some(fraction) = output.replay_seek_to {
            self.seek_replay(fraction);
        }
        if let Some(speed) = output.replay_set_speed {
            self.set_replay_speed(speed);
        }
        if let Some(selected) = investigation_select_row {
            self.with_active_viewer_mut(|viewer| {
                viewer.investigation.set_single_selection(selected.clone());
            });
            self.maybe_auto_seek_to_row(&selected);
        }
        if let Some(hovered) = investigation_hover_row {
            self.with_active_viewer_mut(|viewer| {
                viewer.investigation.hovered_row = Some(hovered);
            });
        } else {
            self.with_active_viewer_mut(|viewer| {
                viewer.investigation.hovered_row = None;
            });
        }

        if needs_preview_refresh {
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
        self.hotpixel_detection.reset();
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

    fn sync_investigation_layers(&mut self) {
        let datasets: Vec<_> = self.host_view_registry.datasets().cloned().collect();
        self.with_active_viewer_mut(|viewer| {
            viewer.investigation.upsert_authoritative_layer(
                RAW_EVENTS_ON_LAYER_ID,
                crate::investigation::InvestigationLayerStyle {
                    title: "Raw Events ON".into(),
                    visible: true,
                    color: RAW_EVENTS_ON_COLOR,
                    marker_shape: augur_plugin_api::HostMarkerShape::Point,
                    size: 2.0,
                },
            );
            viewer.investigation.upsert_authoritative_layer(
                RAW_EVENTS_OFF_LAYER_ID,
                crate::investigation::InvestigationLayerStyle {
                    title: "Raw Events OFF".into(),
                    visible: true,
                    color: RAW_EVENTS_OFF_COLOR,
                    marker_shape: augur_plugin_api::HostMarkerShape::Point,
                    size: 2.0,
                },
            );
            for dataset in &datasets {
                let schema = match &dataset.descriptor.kind {
                    HostDatasetKind::TableV1(schema) => Some(schema),
                    HostDatasetKind::Image2dV1 | HostDatasetKind::Series1dV1 => None,
                };
                viewer
                    .investigation
                    .sync_dataset_layer(&dataset.descriptor, schema);
            }
        });
    }

    fn build_investigation_points_2d(&mut self) -> Vec<Investigation2dPoint> {
        self.sync_active_analysis_roi();
        self.sync_investigation_layers();

        let investigation = self.with_active_viewer(|viewer| viewer.investigation.clone());
        let frame_window = self
            .latest_frame
            .as_ref()
            .map(|frame| (frame.window_start_us, frame.window_end_us));
        let datasets: Vec<_> = self.host_view_registry.datasets().cloned().collect();
        let mut points = Vec::new();

        for dataset in datasets {
            let HostDatasetKind::TableV1(schema) = &dataset.descriptor.kind else {
                continue;
            };
            if schema.coordinate_space_2d.is_none() {
                continue;
            }

            let layer_id = dataset_layer_id(&dataset.descriptor, Some(schema));
            let Some(style) = investigation.layer_styles.get(&layer_id) else {
                continue;
            };
            if !investigation.layer_visible(&layer_id, style.visible) {
                continue;
            }

            self.ensure_host_view_dataset_cached(&dataset.descriptor.id);
            let cache_entry = self.host_view_dataset_cache.get(&dataset.descriptor.id);
            let generation = cache_entry.map(|entry| entry.generation).unwrap_or(0);
            let Ok(Some(table)) = cached_table(cache_entry) else {
                continue;
            };

            let rows = filtered_row_indices(&investigation, &dataset.descriptor.id, schema, table);
            let rows = match frame_window {
                Some(window) => retain_rows_in_frame_span(rows, schema, table, window),
                None => rows,
            };
            for row in rows {
                let Some([x, y]) = coordinate_2d_for_row(schema, table, row) else {
                    continue;
                };
                points.push(Investigation2dPoint {
                    position: [x, y],
                    color: style.color,
                    marker_shape: style.marker_shape,
                    size: style.size,
                    item_key: row_key_for_row(
                        &dataset.descriptor.id,
                        generation,
                        schema,
                        table,
                        row,
                    ),
                    label: style.title.clone(),
                    layer_id: layer_id.clone(),
                });
            }
        }

        points
    }

    fn candidate_event_id(timestamp: u64, x: u16, y: u16, polarity: bool, occurrence: u32) -> u64 {
        timestamp
            ^ u64::from(x).rotate_left(11)
            ^ u64::from(y).rotate_left(23)
            ^ u64::from(polarity as u8).rotate_left(37)
            ^ u64::from(occurrence).rotate_left(47)
    }

    fn row_index_for_key(
        dataset_id: &str,
        generation: u64,
        schema: &augur_plugin_api::TableSchema,
        table: &TableDatasetV1,
        key: &StableRowKey,
    ) -> Option<usize> {
        (0..table.row_count())
            .find(|row| row_key_for_row(dataset_id, generation, schema, table, *row) == *key)
    }

    fn resolve_related_event_ids_for_row(
        &mut self,
        row_key: &StableRowKey,
        visited: &mut HashSet<StableRowKey>,
        out: &mut HashSet<u64>,
    ) {
        if !visited.insert(row_key.clone()) {
            return;
        }
        self.ensure_host_view_dataset_cached(&row_key.dataset_id);
        let Some(dataset) = self
            .host_view_registry
            .dataset(&row_key.dataset_id)
            .cloned()
        else {
            return;
        };
        let HostDatasetKind::TableV1(schema) = &dataset.descriptor.kind else {
            return;
        };
        let cache_entry = self.host_view_dataset_cache.get(&row_key.dataset_id);
        let generation = cache_entry.map(|entry| entry.generation).unwrap_or(0);
        let Ok(Some(table)) = cached_table(cache_entry) else {
            return;
        };
        let Some(row_idx) =
            Self::row_index_for_key(&row_key.dataset_id, generation, schema, table, row_key)
        else {
            return;
        };

        if let Some(event_ids) = table
            .column("event_id")
            .and_then(|column| match &column.values {
                TableColumnValues::U64(values) => values.get(row_idx).copied(),
                _ => None,
            })
        {
            out.insert(event_ids);
            return;
        }

        let pending_relations: Vec<_> = dataset
            .descriptor
            .relations
            .iter()
            .filter_map(|relation| {
                let source_values = table
                    .column(&relation.via_column)
                    .map(|column| &column.values)?;
                let source_value = Self::table_value_key(source_values, row_idx)?;
                Some((
                    relation.target_dataset_id.clone(),
                    relation.target_column.clone(),
                    source_value,
                ))
            })
            .collect();

        for (target_dataset_id, target_column, source_value) in pending_relations {
            self.ensure_host_view_dataset_cached(&target_dataset_id);
            let Some(target_dataset) = self.host_view_registry.dataset(&target_dataset_id).cloned()
            else {
                continue;
            };
            let HostDatasetKind::TableV1(target_schema) = &target_dataset.descriptor.kind else {
                continue;
            };
            let target_cache = self.host_view_dataset_cache.get(&target_dataset_id);
            let target_generation = target_cache.map(|entry| entry.generation).unwrap_or(0);
            let Ok(Some(target_table)) = cached_table(target_cache) else {
                continue;
            };
            let Some(target_values) = target_table
                .column(&target_column)
                .map(|column| &column.values)
            else {
                continue;
            };
            let target_keys: Vec<_> = (0..target_table.row_count())
                .filter(|target_row| {
                    Self::table_value_key(target_values, *target_row).as_deref()
                        == Some(source_value.as_str())
                })
                .map(|target_row| {
                    row_key_for_row(
                        &target_dataset_id,
                        target_generation,
                        target_schema,
                        target_table,
                        target_row,
                    )
                })
                .collect();
            for target_key in target_keys {
                self.resolve_related_event_ids_for_row(&target_key, visited, out);
            }
        }
    }

    /// Resolve the current selection to stable accepted-event ids. Row types
    /// that already expose `event_id` contribute directly; derived rows walk
    /// their declared `HostDatasetRelation`s until they reach event rows.
    fn resolve_selection_event_ids(&mut self) -> HashSet<u64> {
        let selected: Vec<StableRowKey> = self.with_active_viewer(|viewer| {
            viewer.investigation.selected_rows.iter().cloned().collect()
        });
        if selected.is_empty() {
            return HashSet::new();
        }

        let mut result = HashSet::new();
        let mut visited = HashSet::new();
        for key in selected {
            self.resolve_related_event_ids_for_row(&key, &mut visited, &mut result);
        }
        result
    }

    fn build_investigation_scene_3d(&mut self) -> Investigation3dScene {
        self.sync_active_analysis_roi();
        self.sync_investigation_layers();

        let selected_event_ids = self.resolve_selection_event_ids();
        let investigation = self.with_active_viewer(|viewer| viewer.investigation.clone());
        let frame_window = self
            .latest_frame
            .as_ref()
            .map(|frame| (frame.window_start_us, frame.window_end_us));
        let views: Vec<_> = self.host_view_registry.views().cloned().collect();
        let mut layers = Vec::new();
        let mut focus_volume = None;

        for view in views {
            let HostViewKind::Scatter3dFromTable {
                x_column,
                y_column,
                z_column,
            } = &view.descriptor.kind
            else {
                continue;
            };

            let Some(dataset) = self
                .host_view_registry
                .dataset(&view.descriptor.dataset_id)
                .cloned()
            else {
                continue;
            };
            let HostDatasetKind::TableV1(schema) = &dataset.descriptor.kind else {
                continue;
            };

            self.ensure_host_view_dataset_cached(&view.descriptor.dataset_id);
            let cache_entry = self
                .host_view_dataset_cache
                .get(&view.descriptor.dataset_id);
            let generation = cache_entry.map(|entry| entry.generation).unwrap_or(0);
            let Ok(Some(table)) = cached_table(cache_entry) else {
                continue;
            };

            let layer_id = dataset_layer_id(&dataset.descriptor, Some(schema));
            let Some(style) = investigation.layer_styles.get(&layer_id) else {
                continue;
            };
            if !investigation.layer_visible(&layer_id, style.visible) {
                continue;
            }

            let mut points = Vec::new();
            let rows =
                filtered_row_indices(&investigation, &view.descriptor.dataset_id, schema, table);
            let rows = match frame_window {
                Some(window) => retain_rows_in_frame_span(rows, schema, table, window),
                None => rows,
            };
            for row in rows {
                let Some([x, y, z]) = coordinate_3d_for_row(
                    schema,
                    table,
                    row,
                    Some((x_column.as_str(), y_column.as_str(), z_column.as_str())),
                ) else {
                    continue;
                };
                points.push(Investigation3dPoint {
                    position: [x as f32, y as f32, z as f32],
                    color: style.color,
                    size: style.size,
                    item_key: Some(row_key_for_row(
                        &view.descriptor.dataset_id,
                        generation,
                        schema,
                        table,
                        row,
                    )),
                    label: view.descriptor.title.clone(),
                });
            }

            layers.push(Investigation3dLayer {
                id: layer_id,
                title: view.descriptor.title,
                visible: true,
                points,
            });
        }

        let raw_layer_ids = [RAW_EVENTS_ON_LAYER_ID, RAW_EVENTS_OFF_LAYER_ID];
        let raw_layers_visible = raw_layer_ids
            .iter()
            .any(|layer_id| investigation.layer_visible(layer_id, true));
        if raw_layers_visible {
            let (raw_events, effective_time_window_ms, active_roi, on_style, off_style) = self
                .with_active_viewer(|viewer| {
                    let raw_summary = viewer.workspace.point_cloud.visible_summary();
                    (
                        raw_summary.events,
                        raw_summary.effective_time_window_ms,
                        viewer.investigation.active_analysis_roi.clone(),
                        viewer
                            .investigation
                            .layer_styles
                            .get(RAW_EVENTS_ON_LAYER_ID)
                            .cloned(),
                        viewer
                            .investigation
                            .layer_styles
                            .get(RAW_EVENTS_OFF_LAYER_ID)
                            .cloned(),
                    )
                });
            let sensor_height = self
                .latest_frame
                .as_ref()
                .map(|frame| frame.height)
                .unwrap_or(self.sensor_height);
            let sensor_width = self
                .latest_frame
                .as_ref()
                .map(|frame| frame.width)
                .unwrap_or(self.sensor_width);
            let focus_roi = active_roi
                .filter(|roi| !roi_is_effectively_full_frame(roi, sensor_width, sensor_height));
            let earliest_timestamp = raw_events.first().map(|event| event.timestamp).unwrap_or(0);

            let latest_timestamp = raw_events.last().map(|event| event.timestamp).unwrap_or(0);
            if let Some(roi) = focus_roi.as_ref() {
                focus_volume = Some(raw_event_focus_volume(
                    roi,
                    earliest_timestamp,
                    latest_timestamp,
                    sensor_height,
                    effective_time_window_ms,
                ));
            }
            for (layer_id, polarity, style) in [
                (RAW_EVENTS_ON_LAYER_ID, true, on_style),
                (RAW_EVENTS_OFF_LAYER_ID, false, off_style),
            ] {
                let Some(style) = style else {
                    continue;
                };
                if !investigation.layer_visible(layer_id, style.visible) {
                    continue;
                }

                let selection_active = !selected_event_ids.is_empty();
                let mut seen_occurrences = HashMap::new();
                let raw_events_with_ids: Vec<_> = raw_events
                    .iter()
                    .copied()
                    .map(|event| {
                        let occurrence = seen_occurrences
                            .entry((event.timestamp, event.x, event.y, event.polarity))
                            .or_insert(0u32);
                        let event_id = Self::candidate_event_id(
                            event.timestamp,
                            event.x,
                            event.y,
                            event.polarity,
                            *occurrence,
                        );
                        *occurrence = occurrence.saturating_add(1);
                        (event, event_id)
                    })
                    .collect();
                let points: Vec<_> = raw_events_with_ids
                    .iter()
                    .copied()
                    .filter(|(event, _)| event.polarity == polarity)
                    .map(|(event, event_id)| {
                        let inside_focus = focus_roi
                            .as_ref()
                            .is_none_or(|roi| roi.contains(f64::from(event.x), f64::from(event.y)));
                        let is_selected =
                            selection_active && selected_event_ids.contains(&event_id);
                        let emphasised = inside_focus && (!selection_active || is_selected);
                        let mut color = style.color;
                        color[3] = if emphasised {
                            style.color[3]
                        } else {
                            style.color[3].min(36)
                        };
                        Investigation3dPoint {
                            position: raw_event_point_position(
                                event,
                                latest_timestamp,
                                sensor_height,
                                effective_time_window_ms,
                            ),
                            color,
                            size: if emphasised {
                                style.size.max(1.5) * 1.05
                            } else {
                                style.size.max(1.0) * 0.85
                            },
                            item_key: None,
                            label: style.title.clone(),
                        }
                    })
                    .collect();

                layers.push(Investigation3dLayer {
                    id: layer_id.into(),
                    title: style.title,
                    visible: true,
                    points,
                });
            }
        }

        Investigation3dScene {
            layers,
            focus_volume,
        }
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

        reset_provider_for_dataset(&dataset, |index| {
            if let Some(plugin) = self
                .plugin_manager
                .records_mut()
                .get_mut(index)
                .and_then(|record| record.plugin_mut())
            {
                plugin.reset();
            }
        });

        self.mark_host_view_datasets_stale();
        self.refresh_host_view_registry();
    }

    /// Resolve the concrete [`HostActionScopePayload`] for a declared scope
    /// against the currently selected rows. Returns `None` if the selection
    /// does not satisfy the scope (button should be hidden / disabled).
    fn resolve_action_scope_payload(
        &self,
        scope: &HostActionScope,
    ) -> Option<HostActionScopePayload> {
        let selected: Vec<StableRowKey> = self.with_active_viewer(|viewer| {
            viewer.investigation.selected_rows.iter().cloned().collect()
        });
        match scope {
            HostActionScope::Dataset { dataset_id } => Some(HostActionScopePayload::Dataset {
                dataset_id: dataset_id.clone(),
            }),
            HostActionScope::Row { dataset_id } => {
                let matches: Vec<_> = selected
                    .iter()
                    .filter(|key| key.dataset_id == *dataset_id)
                    .collect();
                if matches.len() == 1 {
                    Some(HostActionScopePayload::Row {
                        dataset_id: dataset_id.clone(),
                        row_id: matches[0].row_id.clone(),
                    })
                } else {
                    None
                }
            }
            HostActionScope::Cluster {
                dataset_id,
                group_column,
            } => {
                let matches: Vec<_> = selected
                    .iter()
                    .filter(|key| key.dataset_id == *dataset_id)
                    .collect();
                if matches.is_empty() {
                    return None;
                }
                let resolved = self.host_view_registry.dataset(dataset_id)?;
                let schema = resolved.descriptor.kind.table_schema()?;
                let row_id_column = schema.row_id_column.as_deref()?;
                let entry = self.host_view_dataset_cache.get(dataset_id)?;
                let table = cached_table(Some(entry)).ok()??;
                let id_col = table.column(row_id_column)?;
                let group_col = table.column(group_column)?;
                let mut group_value: Option<String> = None;
                for key in &matches {
                    let mut found_row: Option<usize> = None;
                    for row in 0..table.row_count() {
                        if stable_row_id_value(&id_col.values, row).as_deref()
                            == Some(key.row_id.as_str())
                        {
                            found_row = Some(row);
                            break;
                        }
                    }
                    let row = found_row?;
                    let value = match &group_col.values {
                        TableColumnValues::U64(v) => v.get(row)?.to_string(),
                        TableColumnValues::I64(v) => v.get(row)?.to_string(),
                        TableColumnValues::F64(v) => v.get(row)?.to_string(),
                        TableColumnValues::String(v) => v.get(row)?.clone(),
                        TableColumnValues::Bool(v) => v.get(row)?.to_string(),
                    };
                    match &group_value {
                        Some(existing) if existing != &value => return None,
                        Some(_) => {}
                        None => group_value = Some(value),
                    }
                }
                group_value.map(|value| HostActionScopePayload::Cluster {
                    dataset_id: dataset_id.clone(),
                    group_column: group_column.clone(),
                    group_value: value,
                })
            }
        }
    }

    fn table_value_key(values: &TableColumnValues, row: usize) -> Option<String> {
        match values {
            TableColumnValues::U64(v) => v.get(row).map(ToString::to_string),
            TableColumnValues::I64(v) => v.get(row).map(ToString::to_string),
            TableColumnValues::F64(v) => v.get(row).map(ToString::to_string),
            TableColumnValues::String(v) => v.get(row).cloned(),
            TableColumnValues::Bool(v) => v.get(row).map(ToString::to_string),
        }
    }

    fn table_row_snapshot_json(
        schema: &augur_plugin_api::TableSchema,
        table: &TableDatasetV1,
        row: usize,
    ) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        for column in &schema.columns {
            let Some(column_data) = table.column(&column.id) else {
                continue;
            };
            let value = match &column_data.values {
                TableColumnValues::U64(v) => v.get(row).copied().map(serde_json::Value::from),
                TableColumnValues::I64(v) => v.get(row).copied().map(serde_json::Value::from),
                TableColumnValues::F64(v) => v
                    .get(row)
                    .and_then(|value| serde_json::Number::from_f64(*value))
                    .map(serde_json::Value::Number),
                TableColumnValues::String(v) => v.get(row).cloned().map(serde_json::Value::String),
                TableColumnValues::Bool(v) => v.get(row).copied().map(serde_json::Value::from),
            };
            if let Some(value) = value {
                object.insert(column.id.clone(), value);
            }
        }
        serde_json::Value::Object(object)
    }

    fn cluster_scope_row_snapshots(
        &mut self,
        dataset_id: &str,
        group_column: &str,
        group_value: &str,
    ) -> Option<Vec<serde_json::Value>> {
        self.ensure_host_view_dataset_cached(dataset_id);
        let resolved = self.host_view_registry.dataset(dataset_id)?;
        let schema = resolved.descriptor.kind.table_schema()?;
        let entry = self.host_view_dataset_cache.get(dataset_id)?;
        let table = cached_table(Some(entry)).ok()??;
        let group_values = &table.column(group_column)?.values;
        let mut rows = Vec::new();
        for row in 0..table.row_count() {
            if Self::table_value_key(group_values, row).as_deref() == Some(group_value) {
                rows.push(Self::table_row_snapshot_json(schema, table, row));
            }
        }
        (!rows.is_empty()).then_some(rows)
    }

    fn augment_action_request_params(
        &mut self,
        scope_payload: &HostActionScopePayload,
        params: &mut serde_json::Value,
    ) {
        let HostActionScopePayload::Cluster {
            dataset_id,
            group_column,
            group_value,
        } = scope_payload
        else {
            return;
        };
        let Some(rows) = self.cluster_scope_row_snapshots(dataset_id, group_column, group_value)
        else {
            return;
        };
        if !params.is_object() {
            *params = serde_json::Value::Object(serde_json::Map::new());
        }
        if let Some(object) = params.as_object_mut() {
            object.insert(
                HOST_ACTION_CLUSTER_ROWS_PARAM.into(),
                serde_json::Value::Array(rows),
            );
        }
    }

    fn open_action_modal(
        &mut self,
        action_id: &str,
        title: &str,
        scope_payload: HostActionScopePayload,
        schema: Option<SettingsSchema>,
    ) {
        let params = schema
            .as_ref()
            .map(seed_default_params)
            .unwrap_or(serde_json::Value::Null);
        self.action_modal = Some(ActionModalState {
            action_id: action_id.to_owned(),
            title: title.to_owned(),
            scope_payload,
            schema,
            params,
        });
    }

    fn render_action_modal(&mut self, ctx: &egui::Context) {
        let Some(modal) = self.action_modal.as_ref().cloned() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut updated_params = modal.params.clone();
        egui::Window::new(&modal.title)
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Configure this action, then Apply to queue it.");
                ui.separator();
                if let Some(schema) = &modal.schema {
                    render_action_modal_schema(ui, schema, &mut updated_params);
                } else {
                    ui.label("No parameters — the plugin will use defaults.");
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Apply").clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if let Some(modal) = self.action_modal.as_mut() {
            modal.params = updated_params;
        }
        if apply {
            if let Some(modal) = self.action_modal.take() {
                self.enqueue_action_request(modal.action_id, modal.scope_payload, modal.params);
            }
        } else if cancel || !open {
            self.action_modal = None;
        }
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
                let selected_row = self
                    .with_active_viewer(|viewer| viewer.investigation.primary_selection().cloned());
                let replay_origin_us = self
                    .replay_file_info
                    .as_ref()
                    .map(|info| info.first_timestamp_us);
                let output = match cached_table(cache_entry) {
                    Ok(table) => render_summary_card(
                        ui,
                        schema,
                        table,
                        &dataset.descriptor.empty_message,
                        SummaryCardOptions {
                            dataset_id: &view.descriptor.dataset_id,
                            generation: dataset_generation,
                            selected_row: selected_row.as_ref(),
                            format: TableCellFormatOptions { replay_origin_us },
                            allow_export: true,
                        },
                    ),
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        Default::default()
                    }
                };
                if output.open_full_table {
                    if let Some(table_view_id) = self
                        .host_view_registry
                        .window_views()
                        .find(|v| {
                            v.descriptor.dataset_id == view.descriptor.dataset_id
                                && matches!(v.descriptor.kind, HostViewKind::TableWindow)
                        })
                        .map(|v| v.descriptor.id.clone())
                    {
                        self.host_view_window_open.insert(table_view_id, true);
                    }
                }
                self.handle_host_view_actions(view, output.actions);
            }
            (HostViewKind::TableWindow, HostDatasetKind::TableV1(schema)) => {
                let table_state = self.with_active_viewer(|viewer| {
                    viewer
                        .investigation
                        .table_views
                        .get(&view.descriptor.dataset_id)
                        .cloned()
                        .unwrap_or_default()
                });
                let selected_row = self
                    .with_active_viewer(|viewer| viewer.investigation.primary_selection().cloned());
                let hovered_row =
                    self.with_active_viewer(|viewer| viewer.investigation.hovered_row.clone());
                let output = match cached_table(cache_entry) {
                    Ok(table) => {
                        let filtered_rows = table
                            .map(|table| {
                                self.with_active_viewer(|viewer| {
                                    filtered_row_indices(
                                        &viewer.investigation,
                                        &view.descriptor.dataset_id,
                                        schema,
                                        table,
                                    )
                                })
                            })
                            .unwrap_or_default();
                        render_linked_table_view(
                            ui,
                            schema,
                            table,
                            &table_state,
                            &dataset.descriptor.empty_message,
                            LinkedTableViewOptions {
                                dataset_id: &view.descriptor.dataset_id,
                                generation: dataset_generation,
                                rows: &filtered_rows,
                                selected_row: selected_row.as_ref(),
                                hovered_row: hovered_row.as_ref(),
                                allow_export: true,
                                allow_clear: false,
                                format: TableCellFormatOptions {
                                    replay_origin_us: self
                                        .replay_file_info
                                        .as_ref()
                                        .map(|info| info.first_timestamp_us),
                                },
                            },
                        )
                    }
                    Err(err) => {
                        ui.colored_label(ui.visuals().error_fg_color, err);
                        Default::default()
                    }
                };
                if let Some(selected_row) = output.selected_row {
                    self.with_active_viewer_mut(|viewer| {
                        viewer
                            .investigation
                            .set_single_selection(selected_row.clone());
                    });
                    self.maybe_auto_seek_to_row(&selected_row);
                }
                if let Some(sort_column) = output.sort_column {
                    self.with_active_viewer_mut(|viewer| {
                        viewer
                            .investigation
                            .table_view_state_mut(&view.descriptor.dataset_id)
                            .toggle_sort(&sort_column);
                    });
                }
                if let Some(page_size) = output.page_size {
                    self.with_active_viewer_mut(|viewer| {
                        let state = viewer
                            .investigation
                            .table_view_state_mut(&view.descriptor.dataset_id);
                        state.page_size = page_size;
                        state.page_index = 0;
                    });
                }
                if let Some(page_index) = output.page_index {
                    self.with_active_viewer_mut(|viewer| {
                        viewer
                            .investigation
                            .table_view_state_mut(&view.descriptor.dataset_id)
                            .page_index = page_index;
                    });
                }
                self.handle_host_view_actions(view, output.actions);
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
                    let (table_arc, error_message, generation) = match &cache_entry {
                        Some(HostDatasetCacheEntry {
                            snapshot: Ok(Some(HostDatasetSnapshot::Table(table))),
                            generation,
                        }) => (Some(Arc::clone(table)), None, *generation),
                        Some(HostDatasetCacheEntry {
                            snapshot: Err(err),
                            generation,
                        }) => (None, Some(err.clone()), *generation),
                        _ => (None, None, 0),
                    };
                    let (filtered_rows, table_state, selected_row, hovered_row) = self
                        .with_active_viewer(|viewer| {
                            let filtered_rows = table_arc
                                .as_deref()
                                .map(|table| {
                                    filtered_row_indices(
                                        &viewer.investigation,
                                        &view.descriptor.dataset_id,
                                        schema,
                                        table,
                                    )
                                })
                                .unwrap_or_default();
                            (
                                filtered_rows,
                                viewer
                                    .investigation
                                    .table_views
                                    .get(&view.descriptor.dataset_id)
                                    .cloned()
                                    .unwrap_or_default(),
                                viewer.investigation.primary_selection().cloned(),
                                viewer.investigation.hovered_row.clone(),
                            )
                        });
                    let shared = Arc::new(Mutex::new(TableWindowViewportData {
                        dataset_id: view.descriptor.dataset_id.clone(),
                        generation,
                        schema: schema.clone(),
                        dataset: table_arc,
                        filtered_rows,
                        table_state,
                        selected_row,
                        hovered_row,
                        empty_message: dataset.descriptor.empty_message.clone(),
                        error_message,
                        close_requested: false,
                        export_csv_requested: false,
                        selected_row_requested: None,
                        sort_column_requested: None,
                        page_size_requested: None,
                        page_index_requested: None,
                        replay_origin_us: self
                            .replay_file_info
                            .as_ref()
                            .map(|info| info.first_timestamp_us),
                    }));
                    let shared_for_viewport = Arc::clone(&shared);
                    let window_title = title.clone();
                    let viewport_visuals = ctx.style().visuals.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| {
                            ctx.set_visuals(viewport_visuals.clone());
                            match class {
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
                            }
                        },
                    );

                    let (close, export_csv, selected_row, sort_column, page_size, page_index) = {
                        let Ok(mut data) = shared.lock() else {
                            continue;
                        };
                        let result = (
                            data.close_requested,
                            data.export_csv_requested,
                            data.selected_row_requested.take(),
                            data.sort_column_requested.take(),
                            data.page_size_requested.take(),
                            data.page_index_requested.take(),
                        );
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
                    if let Some(selected_row) = selected_row {
                        self.with_active_viewer_mut(|viewer| {
                            viewer
                                .investigation
                                .set_single_selection(selected_row.clone());
                        });
                        self.maybe_auto_seek_to_row(&selected_row);
                    }
                    if let Some(sort_column) = sort_column {
                        self.with_active_viewer_mut(|viewer| {
                            viewer
                                .investigation
                                .table_view_state_mut(&view.descriptor.dataset_id)
                                .toggle_sort(&sort_column);
                        });
                    }
                    if let Some(page_size) = page_size {
                        self.with_active_viewer_mut(|viewer| {
                            let state = viewer
                                .investigation
                                .table_view_state_mut(&view.descriptor.dataset_id);
                            state.page_size = page_size;
                            state.page_index = 0;
                        });
                    }
                    if let Some(page_index) = page_index {
                        self.with_active_viewer_mut(|viewer| {
                            viewer
                                .investigation
                                .table_view_state_mut(&view.descriptor.dataset_id)
                                .page_index = page_index;
                        });
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
                    let viewport_visuals = ctx.style().visuals.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1200.0, 860.0]),
                        move |ctx, class| {
                            ctx.set_visuals(viewport_visuals.clone());
                            match class {
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
                            }
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
                    let viewport_visuals = ctx.style().visuals.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| {
                            ctx.set_visuals(viewport_visuals.clone());
                            match class {
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
                            }
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
                    let viewport_visuals = ctx.style().visuals.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| {
                            ctx.set_visuals(viewport_visuals.clone());
                            match class {
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
                            }
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
                    let viewport_visuals = ctx.style().visuals.clone();
                    ctx.show_viewport_deferred(
                        viewport_id,
                        egui::ViewportBuilder::default()
                            .with_title(&window_title)
                            .with_inner_size([1100.0, 760.0]),
                        move |ctx, class| {
                            ctx.set_visuals(viewport_visuals.clone());
                            match class {
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
                            }
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
        self.plugin_manager.records().iter().any(|record| {
            record.plugin().is_some_and(|plugin| {
                plugin.enabled() && plugin.input_kind() == PluginInput::RawEvents
            })
        })
    }

    fn plugins_need_retained_event_history(&self) -> bool {
        self.plugin_manager.records().iter().any(|record| {
            record.plugin().is_some_and(|plugin| {
                plugin.enabled() && plugin.capabilities().retained_event_history
            })
        })
    }

    fn raw_events_required(&self) -> bool {
        let preview_mode = self.with_active_viewer(|viewer| viewer.preview_mode);
        let layout = self.active_investigation_layout();
        layout.shows_3d()
            || preview_mode.requires_raw_events()
            || (layout.shows_2d() && self.preview_renderer.prefers_raw_events(preview_mode))
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
        self.plugin_manager
            .records()
            .iter()
            .filter_map(|record| record.plugin())
            .filter(|plugin| plugin.enabled())
            .map(|plugin| plugin.name().to_owned())
            .collect()
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
        let replay_acq_time_ms = self.acq_time_ms;
        let plugin_event_history = self.plugins_need_retained_event_history();
        let open_result = match extension.as_deref() {
            Some("raw") => match RawFileCamera::open(&path) {
                Ok((camera, controls, info)) => {
                    let replay_info = camera.device_info();
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.plugin_event_history = plugin_event_history;
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::default(),
                        replay_pipeline_config(&info, replay_acq_time_ms),
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
                            options.plugin_event_history = plugin_event_history;
                            spawn_pipeline(
                                camera,
                                PackedEventPreviewDecoder::default(),
                                replay_pipeline_config(&info, replay_acq_time_ms),
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
        if let Some(controller) = &self.controller {
            sync_acq_time_atomic(&controller.acq_time_us, self.acq_time_ms);
        }
        self.mask_file = display_mask_file;
        self.replay_notice = replay_notice;
        self.replay_path = Some(path.display().to_string());
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_decoded_events = decoded_events;
        self.replay_paused = false;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = false;
        self.replay_pause_after_seek_target_timestamp_us = None;
        self.replay_pending_fraction = None;
        self.clear_replay_frame_history();
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
        let default_config = replay_pipeline_config(info, self.acq_time_ms);

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
        options.plugin_event_history = self.plugins_need_retained_event_history();
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
        sync_acq_time_atomic(&controller.acq_time_us, self.acq_time_ms);
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
        if !paused && self.replay_has_display_override() {
            self.reopen_replay_at_fraction(self.current_replay_fraction(), false);
            return;
        }

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

    fn preview_histogram_request(&self) -> PreviewHistogramRequest {
        self.with_active_viewer(ViewerState::preview_histogram_request)
    }

    fn apply_aux_window_changes(&mut self, ctx: &egui::Context, aux: ViewerAuxChanges) {
        if aux.contrast_changed || aux.histogram_visibility_changed {
            self.refresh_preview_if_needed(ctx, true);
        }
    }

    fn render_preview_texture_payload(
        &mut self,
        ctx: &egui::Context,
        frame: &PreviewFrame,
    ) -> Result<PreviewDisplayTexture, String> {
        let (preview_mode, time_surface_tau_us) =
            self.with_active_viewer(|viewer| (viewer.preview_mode, viewer.time_surface_tau_us));
        let settings = self.preview_display_settings();
        self.preview_renderer.render(
            PreviewRenderRequest {
                ctx,
                frame,
                settings,
                mode: preview_mode,
                time_surface_tau_us,
            },
            &mut self.preview_perf,
        )
    }

    fn render_preview_texture_from_frame(
        &mut self,
        ctx: &egui::Context,
        frame: &PreviewFrame,
    ) -> Result<(), String> {
        match self.render_preview_texture_payload(ctx, frame) {
            Ok(texture) => {
                self.texture = Some(texture);
                Ok(())
            }
            Err(err) if self.preview_renderer.is_wgpu() => {
                self.preview_renderer = PreviewRenderer::cpu_fallback();
                self.preview_renderer_notice = Some(format!(
                    "WGPU preview renderer failed at runtime and was replaced with the CPU fallback: {err}"
                ));
                let texture = self.render_preview_texture_payload(ctx, frame)?;
                self.texture = Some(texture);
                Ok(())
            }
            Err(err) => Err(err),
        }
    }

    fn refresh_preview_texture_from_latest_frame(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.latest_frame.clone() else {
            return;
        };
        if let Err(err) = self.render_preview_texture_from_frame(ctx, &frame) {
            self.last_error = Some(format!("preview render failed: {err}"));
        }
    }

    fn apply_preview_histogram(&mut self, histogram: Vec<u64>) {
        self.with_active_viewer_mut(|viewer| viewer.apply_histogram(histogram));
    }

    fn apply_preview_auto_histogram(&mut self, histogram: &[u64]) {
        self.with_active_viewer_mut(|viewer| viewer.apply_auto_histogram(histogram));
    }

    fn update_preview_histogram_from_frame(&mut self, frame: &PreviewFrame) {
        let request = self.preview_histogram_request();
        if request == PreviewHistogramRequest::None {
            return;
        }

        let histogram_started = Instant::now();
        let (preview_mode, time_surface_tau_us) =
            self.with_active_viewer(|viewer| (viewer.preview_mode, viewer.time_surface_tau_us));

        match request {
            PreviewHistogramRequest::None => unreachable!("handled by early return above"),
            PreviewHistogramRequest::AutoContrast => {
                let gpu_histogram = match self.preview_renderer.compute_histogram(
                    frame,
                    preview_mode,
                    time_surface_tau_us,
                    request,
                ) {
                    Ok(histogram) => histogram,
                    Err(err) => {
                        self.preview_renderer_notice = Some(format!(
                            "WGPU histogram computation failed; using the CPU histogram fallback instead: {err}"
                        ));
                        None
                    }
                };
                if let Some(histogram) = gpu_histogram {
                    self.apply_preview_auto_histogram(&histogram);
                } else if let Some(histogram) = cached_frame_histogram(frame, preview_mode) {
                    self.apply_preview_auto_histogram(histogram);
                } else {
                    let display_max = self.with_active_viewer(|viewer| {
                        compute_auto_contrast_max(
                            frame,
                            preview_mode,
                            time_surface_tau_us,
                            viewer.contrast_settings.auto_percentile,
                        )
                    });
                    self.with_active_viewer_mut(|viewer| {
                        viewer.apply_auto_contrast_max(display_max);
                    });
                }
            }
            PreviewHistogramRequest::Full => {
                let histogram = match self.preview_renderer.compute_histogram(
                    frame,
                    preview_mode,
                    time_surface_tau_us,
                    request,
                ) {
                    Ok(Some(histogram)) => histogram,
                    Ok(None) => compute_frame_histogram(frame, preview_mode, time_surface_tau_us),
                    Err(err) => {
                        self.preview_renderer_notice = Some(format!(
                            "WGPU histogram computation failed; using the CPU histogram fallback instead: {err}"
                        ));
                        compute_frame_histogram(frame, preview_mode, time_surface_tau_us)
                    }
                };
                self.apply_preview_histogram(histogram);
            }
        }
        self.preview_perf
            .record_histogram(histogram_started.elapsed());
    }

    fn refresh_preview_if_needed(&mut self, ctx: &egui::Context, settings_changed: bool) {
        if !settings_changed || !self.active_investigation_layout().shows_2d() {
            return;
        }
        let Some(frame) = self.latest_frame.clone() else {
            return;
        };
        self.update_preview_histogram_from_frame(&frame);
        if self.mode != AppMode::Idle && !self.external_tool_status().is_streaming() {
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

    fn step_replay(&mut self, ctx: &egui::Context, frame_steps: i64) {
        if self.mode != AppMode::Replaying || frame_steps == 0 {
            return;
        }

        if let Some(target_cursor) = replay_history_step_target(
            self.replay_frame_history.len(),
            self.replay_history_cursor(),
            frame_steps,
        ) {
            if let Some(snapshot) = self.replay_frame_history.get(target_cursor).cloned() {
                self.apply_replay_frame_snapshot(ctx, snapshot, target_cursor);
                return;
            }
        }

        let Some((first_timestamp_us, total_duration_us)) = self
            .replay_file_info
            .as_ref()
            .map(|info| (info.first_timestamp_us, info.total_duration_us))
        else {
            self.last_error = Some("replay file metadata is missing".into());
            return;
        };

        let target_time_us = replay_step_target_time_us(
            self.current_replay_time_us(),
            self.published_acq_time_ms().saturating_mul(1_000),
            frame_steps,
            total_duration_us,
        );
        let target_fraction = replay_fraction_from_time(target_time_us, total_duration_us);
        let controller_active = self.replay_controls.is_some()
            && self
                .controller
                .as_ref()
                .is_some_and(|ctrl| !ctrl.is_stopped());

        if replay_step_uses_current_controller(
            frame_steps,
            self.replay_paused,
            self.replay_finished,
            controller_active,
        ) {
            self.step_replay_forward_from_current(
                first_timestamp_us.saturating_add(target_time_us),
                target_fraction,
            );
            return;
        }

        self.seek_replay(target_fraction);
    }

    fn step_replay_forward_from_current(&mut self, target_timestamp_us: u64, target_fraction: f32) {
        let Some(controls) = self.replay_controls.clone() else {
            return;
        };

        self.set_replay_paused_internal(&controls, false);
        self.replay_paused = true;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = true;
        self.replay_pause_after_seek_target_timestamp_us = Some(target_timestamp_us);
        self.replay_pending_fraction = Some(target_fraction);
        self.last_error = None;
    }

    fn seek_replay(&mut self, fraction: f32) {
        if self.mode != AppMode::Replaying {
            return;
        }

        let desired_paused = self.replay_paused || self.replay_finished;
        self.reopen_replay_at_fraction(fraction, desired_paused);
    }

    /// When replay is paused, seek the transport to the anchor timestamp of
    /// the selected row so the 2D/3D and plugin frames align with it.
    fn maybe_auto_seek_to_row(&mut self, row_key: &StableRowKey) {
        if self.mode != AppMode::Replaying || !self.replay_paused {
            return;
        }
        let Some(info) = self.replay_file_info.clone() else {
            return;
        };
        let schema = {
            let Some(dataset) = self.host_view_registry.dataset(&row_key.dataset_id) else {
                return;
            };
            match &dataset.descriptor.kind {
                HostDatasetKind::TableV1(schema) => schema.clone(),
                _ => return,
            }
        };
        self.ensure_host_view_dataset_cached(&row_key.dataset_id);
        let fraction = (|| -> Option<f32> {
            let entry = self.host_view_dataset_cache.get(&row_key.dataset_id);
            let generation = entry.map(|entry| entry.generation).unwrap_or(0);
            let table = cached_table(entry).ok().flatten()?;
            let row = (0..table.row_count()).find(|row| {
                row_key_for_row(&row_key.dataset_id, generation, &schema, table, *row) == *row_key
            })?;
            let anchor_us = schema.row_anchor_timestamp_us(table, row)?;
            let relative = anchor_us.saturating_sub(info.first_timestamp_us);
            Some(replay_fraction_from_time(relative, info.total_duration_us))
        })();
        if let Some(fraction) = fraction {
            self.seek_replay(fraction);
        }
    }

    fn reopen_replay_at_fraction(&mut self, fraction: f32, desired_paused: bool) {
        let Some(path) = self.replay_path.as_ref().map(PathBuf::from) else {
            self.last_error = Some("replay path is missing".into());
            return;
        };
        let Some(info) = self.replay_file_info.clone() else {
            self.last_error = Some("replay file metadata is missing".into());
            return;
        };
        let decoded_events = self.replay_decoded_events.clone();
        self.clear_replay_frame_history();

        if let Some(controller) = self.controller.take() {
            self.event_store.detach_upstream();
            if let Err(err) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {err}"));
            }
        }

        let data_len = info.data_len();
        let fraction = fraction.clamp(0.0, 1.0);
        let target_rel = ((data_len as f64 * fraction as f64) as u64).min(data_len);
        let target_time_us = replay_time_from_fraction(fraction, info.total_duration_us);
        let target_timestamp_us = info.first_timestamp_us.saturating_add(target_time_us);
        let reopen_result = if let Some(decoded_events) = decoded_events {
            let target_byte = target_rel - (target_rel % PACKED_EVENT_RECORD_BYTES as u64);
            match DecodedEventFileCamera::open_at(decoded_events, &info, target_byte) {
                Ok((camera, controls)) => {
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.plugin_event_history = self.plugins_need_retained_event_history();
                    spawn_pipeline(
                        camera,
                        PackedEventPreviewDecoder::default(),
                        replay_pipeline_config(&info, self.acq_time_ms),
                        options,
                    )
                    .map(|controller| (controller, controls))
                    .map_err(|err| format!("seek pipeline start failed: {err}"))
                }
                Err(err) => Err(format!("seek failed: {err}")),
            }
        } else {
            let target_data_bytes = align_relative_evt3_word_offset(target_rel);
            let target_byte = info.data_offset + target_data_bytes;
            let timestamp_hint_us = info.estimated_timestamp_us_for_data_bytes(target_data_bytes);
            match RawFileCamera::open_at(&path, &info, target_byte) {
                Ok((camera, controls)) => {
                    let mut options = PipelineOptions::preview_only(info.width, info.height);
                    options.plugin_event_history = self.plugins_need_retained_event_history();
                    spawn_pipeline(
                        camera,
                        Evt3CorePreviewDecoder::with_expected_timestamp(timestamp_hint_us),
                        replay_pipeline_config(&info, self.acq_time_ms),
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
                self.replay_pending_fraction = None;
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
        if let Some(controller) = &self.controller {
            sync_acq_time_atomic(&controller.acq_time_us, self.acq_time_ms);
        }
        self.last_preview_process_at = None;
        self.replay_controls = Some(controls);
        self.replay_file_info = Some(info);
        self.replay_paused = desired_paused;
        self.replay_finished = false;
        self.replay_pause_after_seek_frame = pause_after_first_frame;
        self.replay_pause_after_seek_target_timestamp_us =
            pause_after_first_frame.then_some(target_timestamp_us);
        self.replay_pending_fraction = Some(fraction);
        self.last_error = None;
        self.with_active_viewer_mut(ViewerState::clear_session_state);
        self.camera_status = format!(
            "Replaying {}.",
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        );
        reset_preview_render_cache();
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
        self.replay_pause_after_seek_target_timestamp_us = None;
        self.replay_pending_fraction = None;
        self.clear_replay_frame_history();
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
            self.event_store.detach_upstream();
            if let Err(e) = controller.shutdown() {
                self.last_error = Some(format!("pipeline shutdown failed: {e}"));
            }
        }
        if was_replaying {
            self.clear_replay_state();
        }
        self.texture = None;
        self.preview_renderer.reset();
        self.preview_perf.reset();
        self.latest_frame = None;
        self.clear_replay_frame_history();
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
            self.event_store.detach_upstream();
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
        self.replay_pause_after_seek_target_timestamp_us = None;
        self.replay_pending_fraction = None;
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
            sync_acq_time_atomic(&ctrl.acq_time_us, self.acq_time_ms);
            self.acq_dirty = false;
        }

        self.last_error = None;
    }

    fn publish_pending_action_requests(&mut self) {
        if self.pending_action_requests.is_empty() {
            self.persistent_context_data
                .remove(CTX_INVESTIGATION_ACTION_REQUESTS);
            return;
        }
        let queue = HostActionRequestQueue {
            requests: self.pending_action_requests.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&queue) {
            self.persistent_context_data
                .insert(CTX_INVESTIGATION_ACTION_REQUESTS.to_owned(), bytes);
        }
    }

    fn enqueue_action_request(
        &mut self,
        action_id: String,
        scope_payload: HostActionScopePayload,
        mut params: serde_json::Value,
    ) {
        self.augment_action_request_params(&scope_payload, &mut params);
        let request_id = self.next_action_request_id;
        self.next_action_request_id = self.next_action_request_id.saturating_add(1);
        self.pending_action_requests.push(HostActionRequest {
            request_id,
            action_id,
            scope_payload,
            params,
        });
        self.publish_pending_action_requests();
    }

    fn run_analysis(&mut self, frame: &PreviewFrame, append_current_frame_to_event_store: bool) {
        self.analysis_output = AnalysisOutput::default();
        self.plugin_context_data.clear();
        if self.hotpixel_detection.enabled() {
            self.hotpixel_detection
                .process_frame(frame, &mut self.analysis_output);
        }
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
        self.publish_pending_action_requests();
        let retained_history_needed = self.plugins_need_retained_event_history();
        let runtime_plugins_enabled = self
            .plugin_manager
            .records()
            .iter()
            .any(|record| record.plugin().is_some_and(|plugin| plugin.enabled()));
        let ffi_events = if runtime_plugins_enabled {
            let raw_events = frame.events_snapshot().unwrap_or_default();
            let ffi_events: Vec<FfiCdEvent> =
                raw_events.iter().copied().map(FfiCdEvent::from).collect();
            if retained_history_needed {
                let mut synced_upstream = false;
                if let Some((source, cursor)) = self.controller.as_ref().map(|controller| {
                    (
                        controller.event_source.clone(),
                        controller.plugin_event_cursor,
                    )
                }) {
                    self.event_store.attach_upstream(source, cursor);
                    match self.event_store.sync_from_upstream() {
                        Ok(()) => {
                            synced_upstream = cursor.is_some();
                        }
                        Err(err) => {
                            self.analysis_output.warnings.push(AnalysisWarning {
                                source: "Plugin history".into(),
                                severity: AnalysisSeverity::Error,
                                message: err,
                            });
                        }
                    }
                }

                if !synced_upstream && append_current_frame_to_event_store {
                    self.event_store.push_frame(frame);
                }
            } else {
                self.event_store.detach_upstream();
                self.event_store.clear();
            }
            ffi_events
        } else {
            self.event_store.detach_upstream();
            self.event_store.clear();
            Vec::new()
        };

        for phase in [
            PluginInput::FrameOnly,
            PluginInput::RawEvents,
            PluginInput::DerivedData,
        ] {
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

    fn rerun_current_analysis_frame(&mut self, ctx: &egui::Context, reason: &str) -> bool {
        let Some(frame) = self.latest_frame.clone() else {
            self.analysis_notice = Some(format!(
                "{reason} changed, but there is no decoded frame to recompute yet."
            ));
            return false;
        };

        self.host_view_registry_dirty = true;
        self.refresh_host_view_registry_if_dirty();
        self.run_analysis(&frame, false);
        self.mark_host_view_datasets_stale();
        self.analysis_notice = Some(format!(
            "Recomputed the current replay frame after {reason}."
        ));
        ctx.request_repaint();
        true
    }

    fn note_analysis_change(&mut self, ctx: &egui::Context, reason: &str) {
        if self.mode == AppMode::Replaying && self.replay_paused {
            self.rerun_current_analysis_frame(ctx, reason);
        } else {
            self.analysis_notice = Some(format!(
                "{reason} changed. Investigation outputs will refresh on the next processed frame."
            ));
            self.host_view_registry_dirty = true;
            self.mark_host_view_datasets_stale();
            ctx.request_repaint();
        }
    }

    fn handle_investigation_shortcuts(
        &mut self,
        ctx: &egui::Context,
        scene: &Investigation3dScene,
    ) {
        if ctx.wants_keyboard_input() {
            return;
        }

        let (layout_1, layout_2, layout_3, toggle_link, clear_selection, focus_selection) = ctx
            .input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Num1),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Num2),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Num3),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::L),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::F),
                )
            });

        if layout_1 {
            self.set_active_investigation_layout(InvestigationLayout::Preview2dOnly);
            ctx.request_repaint();
        }
        if layout_2 {
            self.set_active_investigation_layout(InvestigationLayout::Split2d3d);
            ctx.request_repaint();
        }
        if layout_3 {
            self.set_active_investigation_layout(InvestigationLayout::Inspection3dOnly);
            ctx.request_repaint();
        }
        if toggle_link {
            self.with_active_viewer_mut(|viewer| {
                viewer.investigation.link_roi_between_2d_and_3d =
                    !viewer.investigation.link_roi_between_2d_and_3d;
            });
            self.sync_active_analysis_roi();
            ctx.request_repaint();
        }
        if clear_selection {
            self.with_active_viewer_mut(|viewer| {
                viewer.investigation.clear_selection();
                viewer.investigation.hovered_row = None;
            });
            ctx.request_repaint();
        }
        if focus_selection {
            let selected =
                self.with_active_viewer(|viewer| viewer.investigation.primary_selection().cloned());
            if let Some(selected) = selected {
                if let Some(target) = scene_point_for_selection(scene, &selected) {
                    self.with_active_viewer_mut(|viewer| {
                        viewer.investigation.camera_focus_target = Some(target);
                        viewer.investigation_3d.focus_on(target);
                    });
                    ctx.request_repaint();
                }
            }
        }
    }

    fn update_preview_texture(&mut self, ctx: &egui::Context) {
        let Some(frame_rx) = self.controller.as_ref().map(|ctrl| ctrl.frame_rx.clone()) else {
            return;
        };

        let dequeue_started = Instant::now();
        let mut newest_frame = None;
        while let Ok(frame) = frame_rx.try_recv() {
            self.with_active_viewer_mut(|viewer| viewer.workspace.point_cloud.push_frame(&frame));
            newest_frame = Some(frame);
        }

        let Some(frame) = newest_frame else {
            return;
        };
        self.preview_perf.record_dequeue(dequeue_started.elapsed());

        let external_streaming = self.external_tool_status().is_streaming();
        let needs_texture = !external_streaming && self.active_investigation_layout().shows_2d();
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

        let waiting_for_seek_target = !replay_seek_target_reached(
            self.replay_pause_after_seek_target_timestamp_us,
            frame.window_end_us,
        );
        if self.mode == AppMode::Replaying
            && self.replay_pause_after_seek_frame
            && waiting_for_seek_target
        {
            ctx.request_repaint_after(process_interval);
            return;
        }

        if self.mode == AppMode::Replaying && self.replay_pause_after_seek_frame {
            if let Some(controls) = &self.replay_controls {
                self.set_replay_paused_internal(controls, true);
            }
            self.replay_pause_after_seek_frame = false;
            self.replay_pause_after_seek_target_timestamp_us = None;
            self.replay_paused = true;
        }
        if self.mode == AppMode::Replaying && self.replay_pending_fraction.is_some() {
            self.replay_pending_fraction = None;
        }

        let frame_total_started = Instant::now();
        let analysis_started = Instant::now();
        self.run_analysis(&frame, true);
        self.host_view_registry_dirty = true;
        self.mark_host_view_datasets_stale();
        self.preview_perf
            .record_analysis(analysis_started.elapsed());

        self.update_preview_histogram_from_frame(&frame);

        let mut line_profile_elapsed = None;
        self.with_active_viewer_mut(|viewer| {
            if viewer.needs_line_profile_refresh() {
                let line_profile_started = Instant::now();
                viewer.line_profile_tool.recompute(&frame);
                line_profile_elapsed = Some(line_profile_started.elapsed());
            }
        });
        if let Some(duration) = line_profile_elapsed {
            self.preview_perf.record_line_profile(duration);
        }

        let external_tool_error = if let Some(tool) = &mut self.external_tool {
            let bridge_started = Instant::now();
            let result = tool.send_frame(&frame, self.nm_per_pixel);
            self.preview_perf
                .record_external_bridge(bridge_started.elapsed());
            result
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
            if let Err(err) = self.render_preview_texture_from_frame(ctx, &frame) {
                self.last_error = Some(format!("preview render failed: {err}"));
            }
        }
        if self.mode == AppMode::Replaying {
            self.record_replay_frame_snapshot(&frame);
        }
        self.latest_frame = Some(frame);
        self.last_preview_process_at = Some(Instant::now());
        self.preview_perf
            .record_frame_total(frame_total_started.elapsed());
    }

    fn settings_are_locked(&self) -> bool {
        self.mode == AppMode::Replaying
            || (self.mode == AppMode::Recording && self.lock_settings_while_recording)
    }

    fn current_replay_byte_fraction(&self) -> f32 {
        let Some(controls) = &self.replay_controls else {
            return 0.0;
        };
        let total_bytes = controls.file_size.saturating_sub(controls.data_offset);
        if total_bytes == 0 {
            return 1.0;
        }

        (controls.bytes_read.load(Ordering::Relaxed) as f32 / total_bytes as f32).clamp(0.0, 1.0)
    }

    fn replay_displayed_time_us(&self) -> Option<u64> {
        let info = self.replay_file_info.as_ref()?;
        if let Some(frame) = &self.latest_frame {
            return Some(
                frame
                    .window_end_us
                    .saturating_sub(info.first_timestamp_us)
                    .min(info.total_duration_us),
            );
        }

        let current_timestamp_us = self
            .replay_controls
            .as_ref()
            .map(|controls| controls.current_timestamp_us.load(Ordering::Relaxed))
            .unwrap_or(0);
        (current_timestamp_us > 0).then_some(
            current_timestamp_us
                .saturating_sub(info.first_timestamp_us)
                .min(info.total_duration_us),
        )
    }

    fn current_replay_fraction(&self) -> f32 {
        let Some(info) = &self.replay_file_info else {
            return 0.0;
        };
        if self.replay_finished {
            return 1.0;
        }
        if let Some(fraction) = self.replay_pending_fraction {
            return fraction.clamp(0.0, 1.0);
        }
        if let Some(time_us) = self.replay_displayed_time_us() {
            return replay_fraction_from_time(time_us, info.total_duration_us);
        }
        self.current_replay_byte_fraction()
    }

    fn current_replay_bytes_read(&self) -> u64 {
        if self.replay_has_display_override() {
            return self
                .replay_history_cursor()
                .and_then(|cursor| self.replay_frame_history.get(cursor))
                .map(|snapshot| snapshot.bytes_read)
                .unwrap_or_else(|| self.replay_controller_bytes_read());
        }

        self.replay_controller_bytes_read()
    }

    fn current_replay_time_us(&self) -> u64 {
        let Some(info) = &self.replay_file_info else {
            return 0;
        };
        replay_time_from_position_sources(
            self.replay_finished,
            self.replay_pending_fraction,
            self.latest_frame.as_ref().map(|frame| frame.window_end_us),
            self.replay_controls
                .as_ref()
                .map(|controls| controls.current_timestamp_us.load(Ordering::Relaxed))
                .unwrap_or(0),
            info.first_timestamp_us,
            info.total_duration_us,
            self.current_replay_byte_fraction(),
        )
    }

    fn active_preview_process_interval(&self) -> Duration {
        process_interval_for_layout(
            self.active_investigation_layout(),
            self.effective_preview_interval_ms(),
            self.point_cloud_interval_ms,
        )
    }

    fn latest_detected_hotpixels(&self) -> Vec<(u16, u16)> {
        self.hotpixel_detection.detected_pixels().to_vec()
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
        self.apply_theme_to_ctx(ctx);
        self.poll_replay_open_task();
        self.poll_tiff_stack_export_task();
        self.update_preview_texture(ctx);
        self.poll_pipeline_state();
        self.refresh_host_view_registry_if_dirty();

        let mode = self.mode;
        let settings_locked = self.settings_are_locked();
        let mut analysis_toggle_changed = false;
        let mut analysis_parameter_changed = false;
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
                    const PREVIEW_RENDERER_TOOLTIP: &str = "Developer-facing diagnostics for the active graphics backend and preview timings. For glow versus wgpu, compare the same replay or live workload with matching preview mode, zoom, and window size.";
                    const REQUESTED_RENDERER_TOOLTIP: &str = "The renderer preference requested through AUGUR_RENDERER. In auto mode this is the preferred backend, not necessarily the one that ended up active.";
                    const ACTIVE_APP_RENDERER_TOOLTIP: &str = "The renderer backend currently used by eframe for the app window. This is the high-level GUI backend, for example glow or wgpu.";
                    const ACTIVE_PREVIEW_RENDERER_TOOLTIP: &str = "The renderer currently generating preview textures. On a wgpu app run this can still fall back to the CPU path if the preview shader pipeline fails.";
                    const BACKEND_TOOLTIP: &str = "The low-level graphics API backend selected by wgpu, such as Metal, Vulkan, D3D12, or the OpenGL compatibility path.";
                    const ADAPTER_TOOLTIP: &str = "The GPU adapter string reported by wgpu for the active device and driver.";
                    const PERF_STAGE_TOOLTIP: &str = "Rolling timings for the accepted preview-frame hot path. Use Frame total as the main glow versus wgpu comparison, then use the rows below it to explain where time goes.";
                    const PERF_LAST_TOOLTIP: &str = "Duration of the most recent sample for this stage in milliseconds.";
                    const PERF_AVG_TOOLTIP: &str = "Rolling arithmetic mean for this stage since app start or the last timing reset.";
                    const PERF_MAX_TOOLTIP: &str = "Slowest observed sample for this stage since app start or the last timing reset.";
                    const FRAME_TOTAL_TOOLTIP: &str = "Primary apples-to-apples comparison metric for glow versus wgpu. This measures the end-to-end CPU-side work for one accepted preview frame after throttling has allowed it through, including analysis, optional derived work, bridge send, and preview rendering when the 2D viewer is active.";
                    const DEQUEUE_TOOLTIP: &str = "Time spent draining pending preview frames and keeping only the newest one. Useful for backlog diagnosis, but not the main glow versus wgpu comparison metric because queue pressure can vary with workload.";
                    const ANALYSIS_TOOLTIP: &str = "Time spent in host-side frame analysis, including built-in analysis and enabled plugin processing.";
                    const HISTOGRAM_TOOLTIP: &str = "Time spent computing the preview histogram when auto-contrast or the histogram window requires it.";
                    const LINE_PROFILE_TOOLTIP: &str = "Time spent recomputing the ON/OFF line profile for the active measurement line.";
                    const CPU_FALLBACK_RENDER_TOOLTIP: &str = "CPU time to convert prepared preview payloads into an egui ColorImage. This is specific to the glow path and CPU fallback path, so treat it as a supporting diagnostic rather than the cross-backend headline number.";
                    const UPLOAD_SUBMIT_TOOLTIP: &str = "CPU-side time to stage preview image or payload data and submit render work to the active backend. This does not wait for GPU completion, so it is useful for pipeline breakdowns but not as the main renderer A/B metric.";
                    const EXTERNAL_BRIDGE_TOOLTIP: &str = "Time spent forwarding the current frame to an external bridge such as ImageJ.";
                    const RESET_PREVIEW_TIMINGS_TOOLTIP: &str = "Clear the rolling preview timing history so the next samples reflect the current replay, mode, zoom, and window size.";
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

                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label("Preview renderer")
                                .on_hover_text(PREVIEW_RENDERER_TOOLTIP);
                            if ui
                                .small_button("Reset timings")
                                .on_hover_text(RESET_PREVIEW_TIMINGS_TOOLTIP)
                                .clicked()
                            {
                                self.preview_perf.reset();
                            }
                        });
                        egui::Grid::new("preview_renderer_info_grid")
                            .num_columns(2)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.small("Requested renderer")
                                    .on_hover_text(REQUESTED_RENDERER_TOOLTIP);
                                ui.monospace(self.renderer_info.requested.label());
                                ui.end_row();

                                ui.small("App renderer")
                                    .on_hover_text(ACTIVE_APP_RENDERER_TOOLTIP);
                                ui.monospace(&self.renderer_info.active_renderer);
                                ui.end_row();

                                ui.small("Preview renderer")
                                    .on_hover_text(ACTIVE_PREVIEW_RENDERER_TOOLTIP);
                                ui.monospace(self.preview_renderer.label());
                                ui.end_row();

                                ui.small("Backend").on_hover_text(BACKEND_TOOLTIP);
                                ui.monospace(&self.renderer_info.backend);
                                ui.end_row();

                                ui.small("Adapter").on_hover_text(ADAPTER_TOOLTIP);
                                ui.monospace(&self.renderer_info.adapter);
                                ui.end_row();
                            });
                        if let Some(notice) = &self.preview_renderer_notice {
                            ui.colored_label(ui.visuals().warn_fg_color, notice);
                        }

                        let perf = self.preview_perf.snapshot();
                        egui::Grid::new("preview_perf_grid")
                            .num_columns(4)
                            .spacing([10.0, 4.0])
                            .show(ui, |ui| {
                                ui.strong("Stage").on_hover_text(PERF_STAGE_TOOLTIP);
                                ui.strong("Last [ms]").on_hover_text(PERF_LAST_TOOLTIP);
                                ui.strong("Avg [ms]").on_hover_text(PERF_AVG_TOOLTIP);
                                ui.strong("Max [ms]").on_hover_text(PERF_MAX_TOOLTIP);
                                ui.end_row();
                                draw_preview_perf_row(
                                    ui,
                                    "Frame total",
                                    FRAME_TOTAL_TOOLTIP,
                                    perf.frame_total,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "Dequeue",
                                    DEQUEUE_TOOLTIP,
                                    perf.dequeue,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "Analysis",
                                    ANALYSIS_TOOLTIP,
                                    perf.analysis,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "Histogram",
                                    HISTOGRAM_TOOLTIP,
                                    perf.histogram,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "Line profile",
                                    LINE_PROFILE_TOOLTIP,
                                    perf.line_profile,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "CPU fallback render",
                                    CPU_FALLBACK_RENDER_TOOLTIP,
                                    perf.cpu_fallback_render,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "Texture stage/submit",
                                    UPLOAD_SUBMIT_TOOLTIP,
                                    perf.upload_submit,
                                );
                                draw_preview_perf_row(
                                    ui,
                                    "External bridge",
                                    EXTERNAL_BRIDGE_TOOLTIP,
                                    perf.external_bridge,
                                );
                            });
                    });

                    if global_settings_changed {
                        self.sync_config_global_from_runtime();
                    }
                });

                // ── View ──────────────────────────────────────────────────────
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.settings_panel_open, "Settings Panel");
                    ui.checkbox(&mut self.analysis_panel_open, "Inspector Panel");
                    let mut scale_bar_show =
                        self.with_active_viewer(|viewer| viewer.scale_bar_settings.show);
                    if ui.checkbox(&mut scale_bar_show, "Show Scale Bar").changed() {
                        self.with_active_viewer_mut(|viewer| {
                            viewer.scale_bar_settings.show = scale_bar_show;
                        });
                    }
                    let mut dark_mode = self.theme_preference.is_dark();
                    if ui
                        .checkbox(&mut dark_mode, "Dark Mode")
                        .on_hover_text("Switch between the light and dark GUI themes.")
                        .changed()
                    {
                        self.set_theme_preference(
                            ctx,
                            UiThemePreference::from_dark_mode(dark_mode),
                        );
                    }
                    ui.separator();
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
                    let mut layout = self.active_investigation_layout();
                    for candidate in [
                        InvestigationLayout::Preview2dOnly,
                        InvestigationLayout::Split2d3d,
                        InvestigationLayout::Inspection3dOnly,
                    ] {
                        let enabled = self.investigation_renderer.is_wgpu()
                            || candidate == InvestigationLayout::Preview2dOnly;
                        let response = ui.add_enabled(
                            enabled,
                            egui::RadioButton::new(layout == candidate, candidate.label()),
                        );
                        if response.clicked() {
                            layout = candidate;
                        }
                    }
                    if layout != self.active_investigation_layout() {
                        self.set_active_investigation_layout(layout);
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
                let has_runtime_plugins = self
                    .plugin_manager
                    .records()
                    .iter()
                    .any(|record| record.plugin().is_some());
                if self.hotpixel_detection.enabled() || has_runtime_plugins {
                    ui.menu_button("Analysis", |ui| {
                        let mut hotpixel_enabled = self.hotpixel_detection.enabled();
                        if ui
                            .checkbox(&mut hotpixel_enabled, "Hotpixel Detection")
                            .changed()
                        {
                            self.hotpixel_detection.set_enabled(hotpixel_enabled);
                            analysis_toggle_changed = true;
                        }

                        for record in self.plugin_manager.records_mut() {
                            let Some(plugin) = record.plugin_mut() else {
                                continue;
                            };
                            let mut enabled = plugin.enabled();
                            if ui.checkbox(&mut enabled, plugin.name()).changed() {
                                plugin.set_enabled(enabled);
                                analysis_toggle_changed = true;
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
                    let mut layout = self.active_investigation_layout();
                    let split = ui.selectable_value(
                        &mut layout,
                        InvestigationLayout::Split2d3d,
                        "2D+3D",
                    );
                    let only_3d = ui.add_enabled(
                        self.investigation_renderer.is_wgpu(),
                        egui::SelectableLabel::new(
                            layout == InvestigationLayout::Inspection3dOnly,
                            "3D",
                        ),
                    );
                    if only_3d.clicked() {
                        layout = InvestigationLayout::Inspection3dOnly;
                    }
                    let only_2d = ui.selectable_value(
                        &mut layout,
                        InvestigationLayout::Preview2dOnly,
                        "2D",
                    );
                    if only_2d.changed() || only_3d.clicked() || split.changed() {
                        self.set_active_investigation_layout(layout);
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
                    analysis_toggle_changed = true;
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

        if analysis_toggle_changed {
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
        let hotpixel_enabled = self.hotpixel_detection.enabled();
        let has_analysis_extensions = hotpixel_enabled || !enabled_plugin_names.is_empty();
        if self.analysis_panel_open {
            egui::SidePanel::right("analysis")
                .min_width(320.0)
                .default_width(380.0)
                .max_width(500.0)
                .show_separator_line(true)
                .show(ctx, |ui| {
                    let _panel_width = constrain_ui_to_clip_width(ui);
                    ui.style_mut().wrap = Some(true);
                    ui.vertical(|ui| {
                        constrain_ui_to_clip_width(ui);
                        ui.horizontal(|ui| {
                            ui.heading("Investigation Inspector");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                egui::RichText::new("▶")
                                                    .size(14.0)
                                                    .color(ui.visuals().weak_text_color()),
                                            )
                                            .frame(false)
                                            .min_size(egui::vec2(20.0, 20.0)),
                                        )
                                        .clicked()
                                    {
                                        self.analysis_panel_open = false;
                                    }
                                },
                            );
                        });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_source("analysis_panel_scroll")
                            .hscroll(false)
                            .horizontal_scroll_offset(0.0)
                            .auto_shrink([true, false])
                            .show(ui, |ui| {
                                constrain_ui_to_clip_width(ui);
                                ui.style_mut().wrap = Some(true);
                                self.render_investigation_inspector(ui);

                                if has_analysis_extensions {
                                    ui.separator();
                                    ui.heading("Analysis Extensions");
                                    ui.small(
                                        "Only enabled analysis modules appear here. Hidden modules stay out of the way.",
                                    );
                                    ui.separator();

                                    if hotpixel_enabled {
                                        let mut stage_changed = false;
                                        egui::Frame::group(ui.style()).show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                ui.heading("Hotpixel Detection");
                                                stage_dirty_badge(
                                                    ui,
                                                    self.hotpixel_detection.is_dirty(),
                                                );
                                            });
                                            ui.small("Phase: host frame analysis");
                                            ui.small(format!(
                                                "Metrics: {} hot pixels detected in the last frame",
                                                self.hotpixel_detection.detected_pixels().len()
                                            ));
                                            ui.separator();
                                            stage_changed =
                                                self.hotpixel_detection.render_ui(ui, false);
                                        });
                                        analysis_parameter_changed |= stage_changed;
                                        if !enabled_plugin_names.is_empty() {
                                            ui.separator();
                                        }
                                    }

                                    let runtime_count = self.plugin_manager.records().len();
                                    for index in 0..runtime_count {
                                        let provider = HostViewProviderKey::Runtime(index);
                                        let dataset_count = self
                                            .host_view_registry
                                            .datasets()
                                            .filter(|dataset| dataset.provider == provider)
                                            .count();
                                        let panel_view_count = self
                                            .host_view_registry
                                            .panel_views_for_provider(provider)
                                            .count();
                                        let rendered = {
                                            let phase_label =
                                                self.plugin_manager.records()[index].phase_label();
                                            let record =
                                                &mut self.plugin_manager.records_mut()[index];
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
                                            let mut stage_changed = false;
                                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.heading(plugin.name());
                                                    stage_dirty_badge(ui, plugin.is_dirty());
                                                });
                                                if !plugin.description().is_empty() {
                                                    ui.weak(plugin.description());
                                                }
                                                ui.small(format!("Phase: {phase_label}"));
                                                ui.small(format!(
                                                    "Metrics: {dataset_count} datasets, {panel_view_count} panel views"
                                                ));
                                                ui.small(format!(
                                                    "Dependencies: {}",
                                                    if plugin.dependencies().is_empty() {
                                                        "none".to_owned()
                                                    } else {
                                                        plugin.dependencies().join(", ")
                                                    }
                                                ));
                                                if !missing_dependencies.is_empty() {
                                                    ui.colored_label(
                                                        ui.visuals().warn_fg_color,
                                                        format!(
                                                            "Missing dependency: {}",
                                                            missing_dependencies.join(", ")
                                                        ),
                                                    );
                                                }
                                                ui.separator();
                                                match render_plugin_settings(ui, plugin, false) {
                                                    Ok(changed) => {
                                                        stage_changed = changed;
                                                    }
                                                    Err(err) => {
                                                        ui.colored_label(
                                                            ui.visuals().error_fg_color,
                                                            format!(
                                                                "Dynamic plugin UI failed: {err}"
                                                            ),
                                                        );
                                                    }
                                                }
                                            });
                                            analysis_parameter_changed |= stage_changed;
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
                                }
                            });
                    });
                });
        } else {
            egui::SidePanel::right("analysis_collapsed")
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
                                    analysis_toggle_changed = true;
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
                    analysis_toggle_changed = true;
                }
                Err(err) => self.last_error = Some(err),
            }
        }

        if rescan_requested {
            match self.plugin_manager.scan_and_load() {
                Ok(()) => {
                    self.last_error = None;
                    self.reset_analysis();
                    analysis_toggle_changed = true;
                }
                Err(err) => self.last_error = Some(err),
            }
        }

        if open_dir_requested {
            if let Err(err) = self.plugin_manager.open_plugins_dir() {
                self.last_error = Some(err);
            }
        }

        if analysis_toggle_changed {
            self.analysis_output = AnalysisOutput::default();
            self.note_analysis_change(ctx, "analysis stage enablement");
        }
        if analysis_parameter_changed {
            self.note_analysis_change(ctx, "analysis parameters");
        }

        let external_status = self.external_tool_status();
        let external_streaming = external_status.is_streaming();
        let external_streaming_label = self.current_external_streaming_label();
        let pipeline_stats = self
            .controller
            .as_ref()
            .map(PipelineController::stats_snapshot);
        let detected_hotpixels = self.latest_detected_hotpixels();
        let mut main_viewer_output: Option<ViewerOutput> = None;
        let mut main_3d_output = None;
        let mut return_preview_to_main = false;
        let mut transport_output = ViewerOutput::default();
        let investigation_points_2d = self.build_investigation_points_2d();
        let investigation_scene_3d = self.build_investigation_scene_3d();
        // Auto-fit camera to scene bounds when data first appears
        if !self.viewer.investigation_3d.auto_fitted && !investigation_scene_3d.is_empty() {
            self.viewer
                .investigation_3d
                .fit_to_scene(&investigation_scene_3d);
            self.viewer.investigation_3d.auto_fitted = true;
        }
        self.handle_investigation_shortcuts(ctx, &investigation_scene_3d);

        if self.popup_open {
            self.sync_popup_shared(
                settings_locked,
                external_streaming,
                &external_streaming_label,
                &investigation_points_2d,
                &investigation_scene_3d,
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
                let latest_frame_for_hover = self.latest_frame.clone();
                let main_time_surface_hover_value = self.preview_time_surface_hover_value(
                    self.viewer.preview_mode,
                    self.viewer.workspace.hover_sensor,
                    self.viewer.time_surface_tau_us,
                    latest_frame_for_hover.as_ref(),
                );
                let selected_row = self.viewer.investigation.primary_selection().cloned();
                let replay_state = self.viewer_replay_state();
                let make_input = |viewer_id: &'static str| ViewerInput {
                    texture: self.texture.as_ref(),
                    frame: self.latest_frame.as_ref(),
                    time_surface_hover_value: main_time_surface_hover_value,
                    overlays: &self.analysis_output.overlays,
                    camera_info: self.camera_info.as_ref(),
                    nm_per_pixel: self.nm_per_pixel,
                    config: &self.config,
                    investigation_points_2d: &investigation_points_2d,
                    selected_row: selected_row.as_ref(),
                    mode: self.mode,
                    settings_locked,
                    pipeline_stats: pipeline_stats.as_ref(),
                    replay: replay_state,
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
                    popup_button_tooltip: "Open in separate window",
                    viewer_id,
                };

                if self.mode == AppMode::Replaying {
                    draw_replay_transport(
                        ui,
                        &mut self.viewer,
                        replay_state,
                        "central_transport",
                        &mut transport_output,
                    );
                    ui.separator();
                }

                match self.viewer.investigation.layout {
                    InvestigationLayout::Preview2dOnly => {
                        main_viewer_output =
                            Some(draw_viewer(ctx, ui, &mut self.viewer, make_input("main")));
                    }
                    InvestigationLayout::Inspection3dOnly => {
                        let max_3d_height = ui.available_size().y.min(800.0);
                        main_3d_output = Some(draw_investigation_3d(
                            ui,
                            &mut self.investigation_renderer,
                            &mut self.viewer.investigation_3d,
                            &investigation_scene_3d,
                            selected_row.as_ref(),
                            Some(&mut self.viewer.workspace.point_cloud),
                            max_3d_height,
                        ));
                    }
                    InvestigationLayout::Split2d3d => {
                        let parent_clip = ui.clip_rect();
                        let (left_rect, right_rect) = show_investigation_split(
                            ui,
                            "investigation_split",
                            &mut self.viewer.investigation.split_ratio,
                        );
                        main_viewer_output =
                            Some(show_investigation_pane(ui, left_rect, parent_clip, |ui| {
                                draw_viewer(ctx, ui, &mut self.viewer, make_input("main_split"))
                            }));
                        main_3d_output =
                            Some(show_investigation_pane(ui, right_rect, parent_clip, |ui| {
                                draw_investigation_3d(
                                    ui,
                                    &mut self.investigation_renderer,
                                    &mut self.viewer.investigation_3d,
                                    &investigation_scene_3d,
                                    selected_row.as_ref(),
                                    Some(&mut self.viewer.workspace.point_cloud),
                                    right_rect.height(),
                                )
                            }));
                    }
                }
            }
        });

        if return_preview_to_main {
            self.close_popup_viewer();
        }

        if let Some(ref mut output) = main_viewer_output {
            output.merge(transport_output);
        } else if transport_output.has_replay_actions() {
            main_viewer_output = Some(transport_output);
        }
        if let Some(output) = main_viewer_output {
            self.handle_viewer_output(ctx, output, false);
        }
        if let Some(output) = main_3d_output {
            if let Some(target) = output.focus_target {
                self.viewer.investigation_3d.focus_on(target);
            }
            if let Some(selected) = output.selected {
                self.viewer
                    .investigation
                    .set_single_selection(selected.clone());
                self.maybe_auto_seek_to_row(&selected);
            }
            self.viewer.investigation.hovered_row = output.hovered;
        }
        if !self.popup_open {
            let aux = self.viewer.show_aux_windows(ctx);
            self.apply_aux_window_changes(ctx, aux);
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
            let viewport_visuals = ctx.style().visuals.clone();
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("popup_preview"),
                egui::ViewportBuilder::default()
                    .with_title("Preview \u{2014} AugurRS")
                    .with_inner_size([1280.0, 820.0]),
                move |ctx, class| {
                    ctx.set_visuals(viewport_visuals.clone());
                    match class {
                        egui::viewport::ViewportClass::Deferred => {
                            let mut root_repaint_requested = false;
                            let mut repaint_after = None;
                            if let Ok(mut data) = shared.lock() {
                                egui::CentralPanel::default().show(ctx, |ui| {
                                    let PopupSharedData {
                                        viewer,
                                        investigation_renderer,
                                        investigation_points_2d,
                                        investigation_scene_3d,
                                        texture,
                                        frame,
                                        time_surface_hover_value,
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
                                    let selected_row =
                                        viewer.investigation.primary_selection().cloned();
                                    let make_input = |viewer_id: &'static str| ViewerInput {
                                        texture: texture.as_ref(),
                                        frame: frame.as_ref(),
                                        time_surface_hover_value: *time_surface_hover_value,
                                        overlays,
                                        camera_info: camera_info.as_ref(),
                                        nm_per_pixel: *nm_per_pixel,
                                        config,
                                        investigation_points_2d: investigation_points_2d.as_slice(),
                                        selected_row: selected_row.as_ref(),
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
                                        popup_button_tooltip: "Return viewer to main window",
                                        viewer_id,
                                    };
                                    let mut popup_transport_output = ViewerOutput::default();
                                    if *mode == AppMode::Replaying {
                                        draw_replay_transport(
                                            ui,
                                            viewer,
                                            *replay,
                                            "popup_transport",
                                            &mut popup_transport_output,
                                        );
                                        ui.separator();
                                    }
                                    let mut popup_output = match viewer.investigation.layout {
                                        InvestigationLayout::Preview2dOnly => {
                                            draw_viewer(ctx, ui, viewer, make_input("popup"))
                                        }
                                        InvestigationLayout::Inspection3dOnly => {
                                            let output = draw_investigation_3d(
                                                ui,
                                                investigation_renderer,
                                                &mut viewer.investigation_3d,
                                                investigation_scene_3d,
                                                selected_row.as_ref(),
                                                Some(&mut viewer.workspace.point_cloud),
                                                ui.available_size().y,
                                            );
                                            if let Some(target) = output.focus_target {
                                                viewer.investigation_3d.focus_on(target);
                                            }
                                            if let Some(selected) = output.selected {
                                                viewer.investigation.set_single_selection(selected);
                                            }
                                            viewer.investigation.hovered_row = output.hovered;
                                            ViewerOutput::default()
                                        }
                                        InvestigationLayout::Split2d3d => {
                                            let parent_clip = ui.clip_rect();
                                            let (left_rect, right_rect) = show_investigation_split(
                                                ui,
                                                "popup_investigation_split",
                                                &mut viewer.investigation.split_ratio,
                                            );
                                            let viewer_output = show_investigation_pane(
                                                ui,
                                                left_rect,
                                                parent_clip,
                                                |ui| {
                                                    draw_viewer(
                                                        ctx,
                                                        ui,
                                                        viewer,
                                                        make_input("popup_split"),
                                                    )
                                                },
                                            );
                                            show_investigation_pane(
                                                ui,
                                                right_rect,
                                                parent_clip,
                                                |ui| {
                                                    let output = draw_investigation_3d(
                                                        ui,
                                                        investigation_renderer,
                                                        &mut viewer.investigation_3d,
                                                        investigation_scene_3d,
                                                        selected_row.as_ref(),
                                                        Some(&mut viewer.workspace.point_cloud),
                                                        right_rect.height(),
                                                    );
                                                    if let Some(target) = output.focus_target {
                                                        viewer.investigation_3d.focus_on(target);
                                                    }
                                                    if let Some(selected) = output.selected {
                                                        viewer
                                                            .investigation
                                                            .set_single_selection(selected);
                                                    }
                                                    viewer.investigation.hovered_row =
                                                        output.hovered;
                                                },
                                            );
                                            viewer_output
                                        }
                                    };
                                    popup_output.merge(popup_transport_output);
                                    let aux = viewer.show_aux_windows(ctx);
                                    popup_output.contrast_changed |= aux.contrast_changed;
                                    popup_output.histogram_visibility_changed |=
                                        aux.histogram_visibility_changed;
                                    root_repaint_requested = popup_output.requests_root_update();
                                    output
                                        .get_or_insert_with(Default::default)
                                        .merge(popup_output);
                                    repaint_after = child_viewport_repaint_after(
                                        viewport_stream_active(*mode, *replay),
                                        *replay_open_task_active,
                                        process_interval_for_layout(
                                            viewer.investigation.layout,
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
                                            investigation_renderer,
                                            investigation_points_2d,
                                            investigation_scene_3d,
                                            texture,
                                            frame,
                                            time_surface_hover_value,
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
                                        let selected_row =
                                            viewer.investigation.primary_selection().cloned();
                                        let make_input = |viewer_id: &'static str| ViewerInput {
                                            texture: texture.as_ref(),
                                            frame: frame.as_ref(),
                                            time_surface_hover_value: *time_surface_hover_value,
                                            overlays,
                                            camera_info: camera_info.as_ref(),
                                            nm_per_pixel: *nm_per_pixel,
                                            config,
                                            investigation_points_2d: investigation_points_2d
                                                .as_slice(),
                                            selected_row: selected_row.as_ref(),
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
                                            external_streaming_label: external_streaming_label
                                                .as_str(),
                                            popup_active: true,
                                            popup_button_tooltip: "Return viewer to main window",
                                            viewer_id,
                                        };
                                        let mut popup_transport_output = ViewerOutput::default();
                                        if *mode == AppMode::Replaying {
                                            draw_replay_transport(
                                                ui,
                                                viewer,
                                                *replay,
                                                "popup_embedded_transport",
                                                &mut popup_transport_output,
                                            );
                                            ui.separator();
                                        }
                                        let mut popup_output = match viewer.investigation.layout {
                                            InvestigationLayout::Preview2dOnly => {
                                                draw_viewer(ctx, ui, viewer, make_input("popup"))
                                            }
                                            InvestigationLayout::Inspection3dOnly => {
                                                let output = draw_investigation_3d(
                                                    ui,
                                                    investigation_renderer,
                                                    &mut viewer.investigation_3d,
                                                    investigation_scene_3d,
                                                    selected_row.as_ref(),
                                                    Some(&mut viewer.workspace.point_cloud),
                                                    ui.available_size().y,
                                                );
                                                if let Some(target) = output.focus_target {
                                                    viewer.investigation_3d.focus_on(target);
                                                }
                                                if let Some(selected) = output.selected {
                                                    viewer
                                                        .investigation
                                                        .set_single_selection(selected);
                                                }
                                                viewer.investigation.hovered_row = output.hovered;
                                                ViewerOutput::default()
                                            }
                                            InvestigationLayout::Split2d3d => {
                                                let parent_clip = ui.clip_rect();
                                                let (left_rect, right_rect) =
                                                    show_investigation_split(
                                                        ui,
                                                        "popup_embedded_investigation_split",
                                                        &mut viewer.investigation.split_ratio,
                                                    );
                                                let viewer_output = show_investigation_pane(
                                                    ui,
                                                    left_rect,
                                                    parent_clip,
                                                    |ui| {
                                                        draw_viewer(
                                                            ctx,
                                                            ui,
                                                            viewer,
                                                            make_input("popup_split"),
                                                        )
                                                    },
                                                );
                                                show_investigation_pane(
                                                    ui,
                                                    right_rect,
                                                    parent_clip,
                                                    |ui| {
                                                        let output = draw_investigation_3d(
                                                            ui,
                                                            investigation_renderer,
                                                            &mut viewer.investigation_3d,
                                                            investigation_scene_3d,
                                                            selected_row.as_ref(),
                                                            Some(&mut viewer.workspace.point_cloud),
                                                            right_rect.height(),
                                                        );
                                                        if let Some(target) = output.focus_target {
                                                            viewer
                                                                .investigation_3d
                                                                .focus_on(target);
                                                        }
                                                        if let Some(selected) = output.selected {
                                                            viewer
                                                                .investigation
                                                                .set_single_selection(selected);
                                                        }
                                                        viewer.investigation.hovered_row =
                                                            output.hovered;
                                                    },
                                                );
                                                viewer_output
                                            }
                                        };
                                        popup_output.merge(popup_transport_output);
                                        let aux = viewer.show_aux_windows(ctx);
                                        popup_output.contrast_changed |= aux.contrast_changed;
                                        popup_output.histogram_visibility_changed |=
                                            aux.histogram_visibility_changed;
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
                    }
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
        self.render_action_modal(ctx);

        self.sync_active_pipeline_requirements();

        let stream_active = pipeline_stream_active(
            self.mode,
            self.replay_paused,
            self.replay_finished,
            self.replay_pause_after_seek_frame,
        );
        let process_interval = self.active_preview_process_interval();
        if self.replay_open_task.is_some() {
            ctx.request_repaint_after(Duration::from_millis(self.preview_interval_ms));
        } else if stream_active && self.controller.is_some() {
            ctx.request_repaint_after(process_interval);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(value) = serde_json::to_string(&self.theme_preference) {
            storage.set_string(UI_THEME_STORAGE_KEY, value);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
}

fn host_marker_shape_label(shape: augur_plugin_api::HostMarkerShape) -> &'static str {
    match shape {
        augur_plugin_api::HostMarkerShape::Circle
        | augur_plugin_api::HostMarkerShape::FilledCircle => "Filled circle",
        augur_plugin_api::HostMarkerShape::Square | augur_plugin_api::HostMarkerShape::Box => "Box",
        augur_plugin_api::HostMarkerShape::Diamond => "Diamond",
        augur_plugin_api::HostMarkerShape::Cross => "Cross",
        augur_plugin_api::HostMarkerShape::Point => "Point",
        augur_plugin_api::HostMarkerShape::Ellipse => "Ellipse",
    }
}

fn stage_dirty_badge(ui: &mut egui::Ui, dirty: bool) {
    let (text, color) = if dirty {
        ("dirty", ui.visuals().warn_fg_color)
    } else {
        ("ready", status_success_color())
    };
    ui.label(egui::RichText::new(text).small().color(color).monospace());
}

fn show_investigation_split(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    split_ratio: &mut f32,
) -> (egui::Rect, egui::Rect) {
    // `available_rect_before_wrap` can extend beyond the current clip rect in panel-heavy
    // layouts, which made the split panes reserve/paint space underneath the right inspector.
    // Clamp the split geometry to the visible clip region so the central workspace cannot
    // overlap side panels.
    let full_rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    if full_rect.width() <= 0.0 || full_rect.height() <= 0.0 {
        return (egui::Rect::NOTHING, egui::Rect::NOTHING);
    }

    let handle_width = 12.0;
    let usable_width = (full_rect.width() - handle_width).max(1.0);
    let min_pane_width = 280.0f32.min(((usable_width - 40.0).max(40.0)) * 0.5);
    let (min_ratio, max_ratio) = investigation_split_ratio_bounds(usable_width, min_pane_width);
    let mut left_width = (usable_width * (*split_ratio).clamp(min_ratio, max_ratio))
        .clamp(min_pane_width, usable_width - min_pane_width);
    let handle_rect = egui::Rect::from_min_size(
        egui::pos2(full_rect.left() + left_width, full_rect.top()),
        egui::vec2(handle_width, full_rect.height()),
    );
    let response = ui.interact(
        handle_rect,
        ui.id().with(id_source),
        egui::Sense::click_and_drag(),
    );
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    if response.dragged() {
        left_width = (left_width + ui.ctx().input(|input| input.pointer.delta().x))
            .clamp(min_pane_width, usable_width - min_pane_width);
        *split_ratio = (left_width / usable_width).clamp(min_ratio, max_ratio);
        ui.ctx().request_repaint();
    }

    let pane_inset = 4.0;
    let left_rect = egui::Rect::from_min_max(
        full_rect.min,
        egui::pos2(handle_rect.left() - pane_inset, full_rect.bottom()),
    );
    let right_rect = egui::Rect::from_min_max(
        egui::pos2(handle_rect.right() + pane_inset, full_rect.top()),
        full_rect.max,
    );
    ui.allocate_rect(full_rect, egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(handle_rect, 4.0, ui.visuals().faint_bg_color);
    painter.line_segment(
        [
            egui::pos2(handle_rect.center().x, handle_rect.top() + 12.0),
            egui::pos2(handle_rect.center().x, handle_rect.bottom() - 12.0),
        ],
        egui::Stroke::new(
            if response.hovered() || response.dragged() {
                2.0
            } else {
                1.0
            },
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ),
    );
    (left_rect, right_rect)
}

fn show_investigation_pane<R>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    parent_clip: egui::Rect,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let pane_clip = rect.intersect(parent_clip);
    ui.allocate_ui_at_rect(rect, |ui| {
        ui.set_clip_rect(pane_clip);
        ui.set_min_width(pane_clip.width().max(0.0));
        ui.set_min_height(pane_clip.height().max(0.0));
        ui.set_max_width(pane_clip.width().max(0.0));
        ui.set_max_height(pane_clip.height().max(0.0));
        ui.set_width(pane_clip.width().max(0.0));
        ui.set_height(pane_clip.height().max(0.0));
        add_contents(ui)
    })
    .inner
}

fn investigation_split_ratio_bounds(usable_width: f32, min_pane_width: f32) -> (f32, f32) {
    if usable_width <= 0.0 {
        return (0.5, 0.5);
    }

    let min_ratio = (min_pane_width / usable_width).clamp(0.0, 0.5);
    (min_ratio, 1.0 - min_ratio)
}

fn clipped_ui_width(available_width: f32, clip_width: f32) -> f32 {
    available_width.min(clip_width).max(0.0)
}

fn constrain_ui_to_clip_width(ui: &mut egui::Ui) -> f32 {
    let width = clipped_ui_width(ui.available_width(), ui.clip_rect().width());
    ui.set_min_width(width);
    ui.set_max_width(width);
    ui.set_width(width);
    width
}

fn scene_point_for_selection(
    scene: &Investigation3dScene,
    selected: &crate::investigation::StableRowKey,
) -> Option<[f32; 3]> {
    scene
        .layers
        .iter()
        .flat_map(|layer| layer.points.iter())
        .find(|point| point.item_key.as_ref() == Some(selected))
        .map(|point| point.position)
}

fn status_success_color() -> egui::Color32 {
    egui::Color32::from_rgb(0, 160, 60)
}

fn analysis_info_color() -> egui::Color32 {
    egui::Color32::from_rgb(30, 100, 220)
}

fn replay_pipeline_config(info: &ReplayFileInfo, acq_time_ms: u64) -> CameraConfig {
    let mut config = CameraConfig::default();
    config.roi.width = info.width;
    config.roi.height = info.height;
    config.global.sensor_width = info.width;
    config.global.sensor_height = info.height;
    config.global.acq_time_ms = acq_time_ms.max(1);
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

fn seed_default_params(schema: &SettingsSchema) -> serde_json::Value {
    use augur_plugin_api::SettingKind;

    let mut map = serde_json::Map::new();
    for section in &schema.sections {
        for item in &section.items {
            let default = match &item.kind {
                SettingKind::Bool { default } => serde_json::json!(*default),
                SettingKind::F64Slider { default, .. } | SettingKind::F64Drag { default, .. } => {
                    serde_json::json!(*default)
                }
                SettingKind::I64Slider { default, .. } | SettingKind::I64Drag { default, .. } => {
                    serde_json::json!(*default)
                }
                SettingKind::Enum { default, .. } => serde_json::json!(*default as u64),
            };
            map.insert(item.key.clone(), default);
        }
    }
    serde_json::Value::Object(map)
}

fn render_action_modal_schema(
    ui: &mut egui::Ui,
    schema: &SettingsSchema,
    params: &mut serde_json::Value,
) {
    use augur_plugin_api::SettingKind;
    use serde_json::json;

    let object = match params.as_object_mut() {
        Some(object) => object,
        None => {
            *params = serde_json::Value::Object(serde_json::Map::new());
            params.as_object_mut().unwrap()
        }
    };

    for section in &schema.sections {
        egui::CollapsingHeader::new(&section.label)
            .default_open(section.default_open)
            .show(ui, |ui| {
                if let Some(description) = &section.description {
                    ui.weak(description);
                }
                for item in &section.items {
                    match &item.kind {
                        SettingKind::Bool { default } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_bool())
                                .unwrap_or(*default);
                            if ui.checkbox(&mut value, &item.label).changed() {
                                object.insert(item.key.clone(), json!(value));
                            }
                        }
                        SettingKind::F64Slider {
                            min,
                            max,
                            default,
                            suffix,
                        } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_f64())
                                .unwrap_or(*default);
                            let mut slider =
                                egui::Slider::new(&mut value, *min..=*max).text(&item.label);
                            if let Some(suffix) = suffix {
                                slider = slider.suffix(suffix);
                            }
                            if ui.add(slider).changed() {
                                object.insert(item.key.clone(), json!(value));
                            }
                        }
                        SettingKind::I64Slider {
                            min,
                            max,
                            default,
                            suffix,
                        } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_i64())
                                .unwrap_or(*default);
                            let mut slider =
                                egui::Slider::new(&mut value, *min..=*max).text(&item.label);
                            if let Some(suffix) = suffix {
                                slider = slider.suffix(suffix);
                            }
                            if ui.add(slider).changed() {
                                object.insert(item.key.clone(), json!(value));
                            }
                        }
                        SettingKind::F64Drag {
                            min,
                            max,
                            speed,
                            default,
                        } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_f64())
                                .unwrap_or(*default);
                            let old = value;
                            ui.horizontal(|ui| {
                                ui.label(&item.label);
                                ui.add(
                                    egui::DragValue::new(&mut value)
                                        .clamp_range(*min..=*max)
                                        .speed(*speed),
                                );
                            });
                            if value != old {
                                object.insert(item.key.clone(), json!(value));
                            }
                        }
                        SettingKind::I64Drag { min, max, default } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_i64())
                                .unwrap_or(*default);
                            let old = value;
                            ui.horizontal(|ui| {
                                ui.label(&item.label);
                                ui.add(egui::DragValue::new(&mut value).clamp_range(*min..=*max));
                            });
                            if value != old {
                                object.insert(item.key.clone(), json!(value));
                            }
                        }
                        SettingKind::Enum { variants, default } => {
                            let mut value = object
                                .get(&item.key)
                                .and_then(|v| v.as_u64())
                                .and_then(|v| usize::try_from(v).ok())
                                .unwrap_or(*default);
                            let old = value;
                            ui.horizontal_wrapped(|ui| {
                                ui.label(&item.label);
                                for (index, variant) in variants.iter().enumerate() {
                                    ui.radio_value(&mut value, index, variant);
                                }
                            });
                            if value != old {
                                object.insert(item.key.clone(), json!(value as u64));
                            }
                        }
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acq_time_us_from_ms, clipped_ui_width, derived_replay_preview_interval_ms,
        investigation_split_ratio_bounds, pipeline_stream_active, raw_event_focus_volume,
        raw_event_point_position, replay_fraction_from_time, replay_history_has_display_override,
        replay_history_step_target, replay_pipeline_config, replay_seek_target_reached,
        replay_snapshot_frame, replay_step_target_time_us, replay_step_uses_current_controller,
        replay_time_from_position_sources, roi_is_effectively_full_frame, sync_acq_time_atomic,
        viewport_stream_active,
    };
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    };

    use crate::{
        investigation::AnalysisRoi,
        viewer_widget::{AppMode, ViewerReplayState},
    };
    use augur_core::{
        metadata::RecordingMetadata,
        pipeline::{CdEvent, LiveEventSource, PreviewFrame},
        replay::ReplayFileInfo,
    };

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

    #[test]
    fn replay_time_source_prefers_pending_fraction_over_stale_frame() {
        assert_eq!(
            replay_time_from_position_sources(false, Some(0.6), Some(350), 420, 100, 1_000, 0.2,),
            600
        );
    }

    #[test]
    fn replay_time_source_prefers_displayed_frame_over_byte_fraction() {
        let time_us =
            replay_time_from_position_sources(false, None, Some(850), 920, 100, 1_000, 0.2);

        assert_eq!(time_us, 750);
        assert!((replay_fraction_from_time(time_us, 1_000) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn paused_seek_keeps_decoding_until_target_frame_is_reached() {
        assert!(!replay_seek_target_reached(Some(1_000), 950));
        assert!(replay_seek_target_reached(Some(1_000), 1_000));
        assert!(replay_seek_target_reached(Some(1_000), 1_050));
    }

    #[test]
    fn paused_seek_without_target_accepts_first_frame() {
        assert!(replay_seek_target_reached(None, 50));
    }

    #[test]
    fn replay_step_target_time_clamps_backward_and_forward() {
        assert_eq!(replay_step_target_time_us(500, 100, 1, 1_000), 600);
        assert_eq!(replay_step_target_time_us(500, 100, -6, 1_000), 0);
        assert_eq!(replay_step_target_time_us(950, 100, 2, 1_000), 1_000);
    }

    #[test]
    fn only_paused_forward_steps_keep_the_current_controller() {
        assert!(replay_step_uses_current_controller(1, true, false, true));
        assert!(!replay_step_uses_current_controller(-1, true, false, true));
        assert!(!replay_step_uses_current_controller(1, false, false, true));
        assert!(!replay_step_uses_current_controller(1, true, true, true));
        assert!(!replay_step_uses_current_controller(1, true, false, false));
    }

    #[test]
    fn replay_history_detects_when_display_is_behind_latest() {
        assert!(replay_history_has_display_override(3, Some(1)));
        assert!(!replay_history_has_display_override(3, Some(2)));
        assert!(!replay_history_has_display_override(0, None));
    }

    #[test]
    fn replay_history_step_target_stays_within_cached_frames() {
        assert_eq!(replay_history_step_target(4, Some(2), -1), Some(1));
        assert_eq!(replay_history_step_target(4, Some(1), 2), Some(3));
        assert_eq!(replay_history_step_target(4, Some(0), -1), None);
        assert_eq!(replay_history_step_target(4, Some(3), 1), None);
    }

    #[test]
    fn replay_snapshot_keeps_upstream_events_without_inline_cache() {
        let events = vec![CdEvent {
            x: 2,
            y: 3,
            timestamp: 42,
            polarity: true,
        }];
        let source = LiveEventSource::default();
        let event_range = source
            .append_cd_frame(&events, 40, 45)
            .expect("test frame fits in default source");
        let frame = PreviewFrame {
            width: 4,
            height: 4,
            pixels: vec![0; 16],
            pixels_on: vec![0; 16],
            pixels_off: vec![0; 16],
            cached_total_histogram: vec![0; 1],
            cached_signed_histogram: vec![0; 1],
            on_count: 1,
            off_count: 0,
            events: Some(events.clone()),
            event_range: Some(event_range),
            event_source: Some(source),
            window_start_us: 40,
            window_end_us: 45,
        };

        let snapshot = replay_snapshot_frame(&frame);

        assert!(snapshot.events.is_none());
        assert_eq!(snapshot.events_snapshot(), Some(events));
    }

    #[test]
    fn raw_event_point_position_flips_sensor_y_and_scales_depth_to_history_window() {
        let point = raw_event_point_position(
            CdEvent {
                x: 12,
                y: 10,
                timestamp: 8_000,
                polarity: true,
            },
            10_000,
            720,
            100.0,
        );

        assert_eq!(point[0], 12.0);
        assert_eq!(point[1], 709.0);
        assert!((point[2] + 14.4).abs() < 1e-4);
    }

    #[test]
    fn raw_event_focus_volume_uses_current_time_span() {
        let focus = raw_event_focus_volume(
            &AnalysisRoi {
                x_min: 10.0,
                x_max: 20.0,
                y_min: 30.0,
                y_max: 50.0,
            },
            8_000,
            10_000,
            720,
            100.0,
        );

        assert_eq!(focus.min[0], 10.0);
        assert_eq!(focus.max[0], 20.0);
        assert_eq!(focus.min[1], 669.0);
        assert_eq!(focus.max[1], 689.0);
        assert!((focus.min[2] + 14.4).abs() < 1e-4);
        assert_eq!(focus.max[2], 0.0);
    }

    #[test]
    fn full_frame_roi_detection_ignores_linked_full_sensor_bounds() {
        let full = AnalysisRoi {
            x_min: 0.0,
            x_max: 1279.0,
            y_min: 0.0,
            y_max: 719.0,
        };
        let partial = AnalysisRoi {
            x_min: 10.0,
            x_max: 100.0,
            y_min: 20.0,
            y_max: 120.0,
        };

        assert!(roi_is_effectively_full_frame(&full, 1280, 720));
        assert!(!roi_is_effectively_full_frame(&partial, 1280, 720));
    }

    #[test]
    fn paused_seek_keeps_stream_updates_alive() {
        assert!(pipeline_stream_active(
            AppMode::Replaying,
            true,
            false,
            true,
        ));
        assert!(!pipeline_stream_active(
            AppMode::Replaying,
            true,
            false,
            false,
        ));
    }

    #[test]
    fn paused_seek_keeps_child_viewports_alive() {
        let replay = ViewerReplayState {
            active: true,
            paused: true,
            finished: false,
            stepping: true,
            ..ViewerReplayState::default()
        };
        assert!(viewport_stream_active(AppMode::Replaying, replay));
    }

    #[test]
    fn replay_pipeline_config_preserves_requested_acquisition_time() {
        let info = ReplayFileInfo {
            file_size: 0,
            data_offset: 0,
            width: 640,
            height: 480,
            metadata: RecordingMetadata::default(),
            total_duration_us: 0,
            first_timestamp_us: 0,
            nominal_bytes_per_sec: None,
        };

        let config = replay_pipeline_config(&info, 125);

        assert_eq!(config.global.acq_time_ms, 125);
        assert_eq!(config.global.sensor_width, 640);
        assert_eq!(config.global.sensor_height, 480);
    }

    #[test]
    fn replay_acquisition_time_sync_writes_microseconds() {
        let acq_time_us = Arc::new(AtomicU64::new(50_000));

        sync_acq_time_atomic(&acq_time_us, 125);

        assert_eq!(
            acq_time_us.load(Ordering::Relaxed),
            acq_time_us_from_ms(125)
        );
    }

    #[test]
    fn split_ratio_bounds_follow_the_actual_min_pane_width() {
        let (min_ratio, max_ratio) = investigation_split_ratio_bounds(1_188.0, 280.0);

        assert!((min_ratio - (280.0 / 1_188.0)).abs() < 1e-6);
        assert!((max_ratio - (1.0 - (280.0 / 1_188.0))).abs() < 1e-6);
    }

    #[test]
    fn split_ratio_bounds_collapse_to_center_when_min_panes_fill_the_width() {
        assert_eq!(investigation_split_ratio_bounds(200.0, 120.0), (0.5, 0.5));
    }

    #[test]
    fn clipped_ui_width_prefers_the_visible_inner_width() {
        assert_eq!(clipped_ui_width(380.0, 362.0), 362.0);
        assert_eq!(clipped_ui_width(280.0, 320.0), 280.0);
        assert_eq!(clipped_ui_width(-5.0, 320.0), 0.0);
    }
}
