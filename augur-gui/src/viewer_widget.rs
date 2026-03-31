use augur_core::{
    analysis::{AnalysisSeverity, AnalysisWarning, Overlay},
    camera::DeviceInfo,
    config::{CameraConfig, RoiConfig},
    pipeline::{PipelineStatsSnapshot, PreviewFrame},
};
use egui_phosphor::regular as phosphor;

use crate::{
    app::PANEL_ROUNDING,
    colormap::Colormap,
    point_cloud::{PointCloudMetrics, PointCloudState},
    preview::{reset_preview_render_cache, PreviewDisplaySettings, PreviewMode},
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
const PREVIEW_ZOOM_MIN: f32 = 1.0;
pub(crate) const PREVIEW_ZOOM_MAX: f32 = 16.0;
pub(crate) const DEFAULT_TIME_SURFACE_TAU_US: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ViewMode {
    Preview2d,
    PointCloud3d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewTool {
    None,
    SelectRoi,
    LineProfile,
    Ruler,
    AnnotateRect,
    AnnotateEllipse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMode {
    Idle,
    Previewing,
    Recording,
    Replaying,
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewWorkspaceState {
    pub(crate) tool: PreviewTool,
    pub(crate) zoom: f32,
    pub(crate) pan: egui::Vec2,
    pub(crate) crop_to_roi: bool,
    pub(crate) hover_sensor: Option<(u16, u16)>,
    selection_anchor: Option<egui::Pos2>,
    pending_roi: Option<egui::Rect>,
    pub(crate) point_cloud: PointCloudState,
}

impl Default for PreviewWorkspaceState {
    fn default() -> Self {
        Self {
            tool: PreviewTool::None,
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
    pub(crate) fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.pending_roi = None;
        self.tool = PreviewTool::None;
    }

    pub(crate) fn clear_session_state(&mut self) {
        self.hover_sensor = None;
        self.selection_anchor = None;
        self.pending_roi = None;
        self.point_cloud.clear();
    }

    pub(crate) fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ViewerOutput {
    pub(crate) roi_committed: bool,
    pub(crate) new_roi: Option<RoiConfig>,
    pub(crate) preview_mode_changed: bool,
    pub(crate) contrast_changed: bool,
    pub(crate) time_surface_tau_changed: bool,
    pub(crate) popup_toggled: bool,
    pub(crate) return_from_external: bool,
    pub(crate) mask_hotpixels_clicked: bool,
    pub(crate) replay_toggle_pause: bool,
    pub(crate) replay_restart: bool,
    pub(crate) replay_stop: bool,
    pub(crate) replay_seek_to: Option<f32>,
    pub(crate) replay_set_speed: Option<f32>,
}

impl ViewerOutput {
    pub(crate) fn needs_preview_refresh(&self) -> bool {
        self.preview_mode_changed || self.contrast_changed || self.time_surface_tau_changed
    }

    pub(crate) fn requests_root_update(&self) -> bool {
        self.popup_toggled
            || self.return_from_external
            || self.mask_hotpixels_clicked
            || self.replay_toggle_pause
            || self.replay_restart
            || self.replay_stop
            || self.replay_seek_to.is_some()
            || self.replay_set_speed.is_some()
            || self.new_roi.is_some()
            || self.needs_preview_refresh()
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.roi_committed |= other.roi_committed;
        if other.new_roi.is_some() {
            self.new_roi = other.new_roi;
        }
        self.preview_mode_changed |= other.preview_mode_changed;
        self.contrast_changed |= other.contrast_changed;
        self.time_surface_tau_changed |= other.time_surface_tau_changed;
        self.popup_toggled |= other.popup_toggled;
        self.return_from_external |= other.return_from_external;
        self.mask_hotpixels_clicked |= other.mask_hotpixels_clicked;
        self.replay_toggle_pause |= other.replay_toggle_pause;
        self.replay_restart |= other.replay_restart;
        self.replay_stop |= other.replay_stop;
        if other.replay_seek_to.is_some() {
            self.replay_seek_to = other.replay_seek_to;
        }
        if other.replay_set_speed.is_some() {
            self.replay_set_speed = other.replay_set_speed;
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewerState {
    pub(crate) view_mode: ViewMode,
    pub(crate) workspace: PreviewWorkspaceState,
    pub(crate) preview_mode: PreviewMode,
    pub(crate) time_surface_tau_us: u64,
    pub(crate) contrast_settings: ContrastSettings,
    pub(crate) histogram_window: HistogramWindow,
    pub(crate) line_profile_tool: LineProfileTool,
    pub(crate) ruler_tool: RulerTool,
    pub(crate) scale_bar_settings: ScaleBarSettings,
    pub(crate) annotation_manager: AnnotationManager,
    replay_seek_drag: Option<f32>,
}

impl Default for ViewerState {
    fn default() -> Self {
        Self {
            view_mode: ViewMode::Preview2d,
            workspace: PreviewWorkspaceState::default(),
            preview_mode: PreviewMode::default(),
            time_surface_tau_us: DEFAULT_TIME_SURFACE_TAU_US,
            contrast_settings: ContrastSettings::default(),
            histogram_window: HistogramWindow::default(),
            line_profile_tool: LineProfileTool::default(),
            ruler_tool: RulerTool::default(),
            scale_bar_settings: ScaleBarSettings::default(),
            annotation_manager: AnnotationManager::default(),
            replay_seek_drag: None,
        }
    }
}

impl ViewerState {
    pub(crate) fn clear_session_state(&mut self) {
        self.workspace.clear_session_state();
        self.replay_seek_drag = None;
    }

    pub(crate) fn set_view_mode(&mut self, view_mode: ViewMode) {
        self.view_mode = view_mode;
        if view_mode != ViewMode::Preview2d {
            self.workspace.clear_selection();
        }
    }

    pub(crate) fn preview_display_settings(&self) -> PreviewDisplaySettings {
        PreviewDisplaySettings {
            display_min: self.contrast_settings.display_min,
            display_max: self.contrast_settings.display_max,
            gamma: self.contrast_settings.gamma,
        }
    }

    pub(crate) fn apply_histogram(&mut self, histogram: Vec<u64>) {
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
    }

    pub(crate) fn show_aux_windows(&mut self, ctx: &egui::Context) -> bool {
        let previous_contrast = self.contrast_settings.clone();
        self.histogram_window
            .show(ctx, &mut self.contrast_settings, self.preview_mode);
        let contrast_changed = self.contrast_settings != previous_contrast;
        self.line_profile_tool.show_window(ctx);
        contrast_changed
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewerReplayState {
    pub(crate) active: bool,
    pub(crate) paused: bool,
    pub(crate) finished: bool,
    pub(crate) speed: f32,
    pub(crate) fraction: f32,
    pub(crate) duration_us: u64,
    pub(crate) time_us: u64,
    pub(crate) bytes_read: u64,
    pub(crate) data_len: u64,
}

pub(crate) struct ViewerInput<'a> {
    pub(crate) texture: Option<&'a egui::TextureHandle>,
    pub(crate) frame: Option<&'a PreviewFrame>,
    pub(crate) overlays: &'a [Overlay],
    pub(crate) camera_info: Option<&'a DeviceInfo>,
    pub(crate) nm_per_pixel: f64,
    pub(crate) config: &'a CameraConfig,
    pub(crate) mode: AppMode,
    pub(crate) settings_locked: bool,
    pub(crate) pipeline_stats: Option<&'a PipelineStatsSnapshot>,
    pub(crate) replay: ViewerReplayState,
    pub(crate) analysis_warnings: &'a [AnalysisWarning],
    pub(crate) analysis_notice: Option<&'a str>,
    pub(crate) detected_hotpixels: &'a [(u16, u16)],
    pub(crate) config_dirty: bool,
    pub(crate) acq_dirty: bool,
    pub(crate) replay_open_task_active: bool,
    pub(crate) replay_notice: Option<&'a str>,
    pub(crate) last_error: Option<&'a str>,
    pub(crate) external_streaming: bool,
    pub(crate) external_streaming_label: &'a str,
    pub(crate) popup_active: bool,
    pub(crate) popup_button_label: &'a str,
    pub(crate) popup_button_tooltip: &'a str,
    pub(crate) viewer_id: &'a str,
}

pub(crate) fn draw_viewer(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: ViewerInput<'_>,
) -> ViewerOutput {
    let _ = input.overlays;
    let mut output = ViewerOutput::default();
    let mut pc_metrics: Option<PointCloudMetrics> = None;

    ui.heading(preview_heading(input.mode, state.view_mode));
    ui.separator();

    if let Some(info) = input.camera_info {
        ui.label(format!(
            "{} | serial: {} | firmware: {}",
            info.compatible.as_deref().unwrap_or(&info.model),
            info.serial.as_deref().unwrap_or("-"),
            info.firmware.as_deref().unwrap_or("-"),
        ));
    }

    let controls_reserve = 190.0;

    if input.external_streaming {
        output.popup_toggled |= draw_preview_toolbar(ui, state, input.popup_active, &input);
        let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
        draw_text_placeholder(ui, max_image_height, input.external_streaming_label);
        ui.add_space(8.0);
        if ui.button("Return to augur").clicked() {
            output.return_from_external = true;
        }
    } else {
        match state.view_mode {
            ViewMode::Preview2d => {
                output.popup_toggled |= draw_preview_toolbar(ui, state, input.popup_active, &input);
                let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
                if let (Some(texture), Some(frame)) = (input.texture, input.frame) {
                    let scale_bar_settings = state.scale_bar_settings.clone();
                    output.merge(draw_preview_canvas(
                        ui,
                        texture,
                        frame,
                        input.config,
                        state,
                        PreviewCanvasOptions {
                            scale_bar_settings,
                            nm_per_pixel: input.nm_per_pixel,
                            settings_locked: input.settings_locked,
                            max_height: max_image_height,
                        },
                    ));
                } else {
                    state.workspace.hover_sensor = None;
                    draw_empty_preview_placeholder(
                        ui,
                        max_image_height,
                        input.mode,
                        state.view_mode,
                    );
                }
            }
            ViewMode::PointCloud3d => {
                output.popup_toggled |= draw_point_cloud_toolbar(ui, state, &input);
                let max_image_height = (ui.available_size().y - controls_reserve).max(180.0);
                pc_metrics = Some(state.workspace.point_cloud.draw(
                    ui,
                    input.config.roi,
                    max_image_height,
                ));
            }
        }
    }

    if input.mode == AppMode::Replaying {
        ui.add_space(4.0);
        draw_replay_transport(ui, state, input.replay, input.viewer_id, &mut output);
    }

    egui::ScrollArea::vertical()
        .max_height(controls_reserve)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.separator();
            draw_viewer_controls(ctx, ui, state, &input, pc_metrics, &mut output);
        });

    output
}

pub(crate) fn draw_text_placeholder(ui: &mut egui::Ui, max_image_height: f32, message: &str) {
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

fn draw_viewer_controls(
    _ctx: &egui::Context,
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
    pc_metrics: Option<PointCloudMetrics>,
    output: &mut ViewerOutput,
) {
    match state.view_mode {
        ViewMode::Preview2d => {
            ui.horizontal(|ui| {
                let previous_preview_mode = state.preview_mode;
                ui.label("Mode");
                egui::ComboBox::from_id_source((input.viewer_id, "preview_mode"))
                    .selected_text(match state.preview_mode {
                        PreviewMode::Intensity(colormap) => {
                            format!("Intensity / {}", colormap.label())
                        }
                        mode => mode.label().to_owned(),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut state.preview_mode,
                            PreviewMode::RedBlue,
                            PreviewMode::RedBlue.label(),
                        );
                        ui.selectable_value(
                            &mut state.preview_mode,
                            PreviewMode::SignedCount,
                            PreviewMode::SignedCount.label(),
                        );
                        ui.selectable_value(
                            &mut state.preview_mode,
                            PreviewMode::TimeSurface,
                            PreviewMode::TimeSurface.label(),
                        );
                        ui.separator();
                        for colormap in Colormap::ALL {
                            ui.selectable_value(
                                &mut state.preview_mode,
                                PreviewMode::Intensity(colormap),
                                format!("Intensity / {}", colormap.label()),
                            );
                        }
                    });
                output.preview_mode_changed |= state.preview_mode != previous_preview_mode;
                if output.preview_mode_changed {
                    reset_preview_render_cache();
                }

                ui.checkbox(&mut state.scale_bar_settings.show, "Scale bar");
                if state.scale_bar_settings.show {
                    egui::ComboBox::from_id_source((input.viewer_id, "scale_bar_position"))
                        .selected_text(match state.scale_bar_settings.position {
                            ScaleBarPosition::TopLeft => "Top left",
                            ScaleBarPosition::TopRight => "Top right",
                            ScaleBarPosition::BottomLeft => "Bottom left",
                            ScaleBarPosition::BottomRight => "Bottom right",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.scale_bar_settings.position,
                                ScaleBarPosition::TopLeft,
                                "Top left",
                            );
                            ui.selectable_value(
                                &mut state.scale_bar_settings.position,
                                ScaleBarPosition::TopRight,
                                "Top right",
                            );
                            ui.selectable_value(
                                &mut state.scale_bar_settings.position,
                                ScaleBarPosition::BottomLeft,
                                "Bottom left",
                            );
                            ui.selectable_value(
                                &mut state.scale_bar_settings.position,
                                ScaleBarPosition::BottomRight,
                                "Bottom right",
                            );
                        });
                }
            });

            if matches!(state.preview_mode, PreviewMode::TimeSurface) {
                ui.horizontal(|ui| {
                    let mut tau_ms = state.time_surface_tau_us as f64 / 1_000.0;
                    let response = ui.add(
                        egui::Slider::new(&mut tau_ms, 1.0..=1_000.0)
                            .text("Decay τ [ms]")
                            .logarithmic(true),
                    );
                    if response.changed() {
                        state.time_surface_tau_us = (tau_ms * 1_000.0).round().max(1.0) as u64;
                        output.time_surface_tau_changed = true;
                    }
                });
                if input.frame.is_some_and(|frame| frame.events.is_none()) {
                    ui.small(
                        "Time Surface needs raw preview events. Augur will fall back to grayscale intensity until a raw-event frame is available.",
                    );
                }
            }

            if let Some(stats) = input
                .frame
                .and_then(|frame| state.annotation_manager.statistics_for_selected(frame))
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

            if !state.annotation_manager.annotations().is_empty() {
                egui::CollapsingHeader::new("Annotations")
                    .id_source((input.viewer_id, "annotations"))
                    .default_open(true)
                    .show(ui, |ui| {
                        for annotation in state.annotation_manager.annotations() {
                            let selected =
                                state.annotation_manager.selected_id() == Some(annotation.id);
                            let _ = ui.selectable_label(selected, &annotation.label);
                        }
                        if ui
                            .add_enabled(
                                state.annotation_manager.selected_id().is_some(),
                                egui::Button::new("Delete selected"),
                            )
                            .clicked()
                        {
                            state.annotation_manager.delete_selected();
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

    if let Some(stats) = input.pipeline_stats {
        ui.label(format!(
            "{:.2} Mev/s  |  {:.2} MB/s  |  {:02}:{:02}:{:02} elapsed",
            stats.mev_per_s,
            stats.mb_per_s,
            (stats.elapsed_s as u64) / 3600,
            (stats.elapsed_s as u64 % 3600) / 60,
            stats.elapsed_s as u64 % 60
        ));
        ui.small(format!(
            "Preview drops: {} packets / {} frames  |  Preview queues HW: {} / {}  |  Disk queue HW: {}  |  Disk wait/write: {:.1} / {:.1} ms",
            stats.preview_packet_drops,
            stats.preview_frame_drops,
            stats.preview_packet_queue_high_water,
            stats.preview_frame_queue_high_water,
            stats.disk_queue_high_water,
            stats.disk_send_wait_us as f64 / 1_000.0,
            stats.disk_write_us as f64 / 1_000.0,
        ));
    }

    if let Some(frame) = input.frame {
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

    if input.mode != AppMode::Idle && input.mode != AppMode::Replaying {
        if input.config_dirty || input.acq_dirty {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "There are unapplied runtime changes.",
            );
        } else {
            ui.label("Runtime settings on the camera are up to date.");
        }
    }

    if !input.analysis_warnings.is_empty() {
        ui.separator();
        for warning in input.analysis_warnings {
            ui.colored_label(
                analysis_warning_color(warning.severity, ui.visuals()),
                format!("{}: {}", warning.source, warning.message),
            );
        }

        if !input.detected_hotpixels.is_empty() {
            let can_copy = !input.settings_locked;
            if ui
                .add_enabled(can_copy, egui::Button::new("Mask detected hotpixels"))
                .clicked()
            {
                output.mask_hotpixels_clicked = true;
            }
            if !can_copy {
                ui.label("Unlock runtime settings to copy detections into the DEM mask.");
            }
        }
    }

    if let Some(notice) = input.analysis_notice {
        ui.label(notice);
    }

    if input.replay_open_task_active {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(input.replay_notice.unwrap_or("Opening replay..."));
        });
    }

    if let Some(err) = input.last_error {
        ui.separator();
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
}

fn draw_preview_toolbar(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    popup_active: bool,
    input: &ViewerInput<'_>,
) -> bool {
    let workspace = &mut state.workspace;
    let line_profile_tool = &mut state.line_profile_tool;
    let ruler_tool = &mut state.ruler_tool;
    let histogram_open = &mut state.histogram_window.open;

    let mut popup_toggled = false;
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
            workspace.clear_selection();
        }

        let mut select_roi_button = ui.add_enabled(
            !input.settings_locked,
            egui::SelectableLabel::new(
                workspace.tool == PreviewTool::SelectRoi,
                toolbar_icon(phosphor::SELECTION),
            ),
        );
        if input.settings_locked {
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
                popup_active,
                toolbar_icon(phosphor::ARROW_SQUARE_OUT),
            ))
            .on_hover_text(input.popup_button_tooltip)
            .clicked()
        {
            popup_toggled = true;
        }

        ui.separator();
        if let (Some((x, y)), Some(frame)) = (workspace.hover_sensor, input.frame) {
            let width = usize::from(frame.width.max(1));
            let idx = usize::from(y) * width + usize::from(x);
            if idx < frame.pixels.len() {
                let full = format!(
                    "x {x}, y {y} | ON: {} OFF: {} Total: {}",
                    frame.pixels_on[idx], frame.pixels_off[idx], frame.pixels[idx]
                );
                let short = format!("x {x}, y {y}");
                let avail = ui.available_width();
                let full_w = ui.fonts(|f| {
                    f.layout_no_wrap(
                        full.clone(),
                        egui::FontId::monospace(
                            ui.style().text_styles[&egui::TextStyle::Monospace].size,
                        ),
                        egui::Color32::WHITE,
                    )
                    .size()
                    .x
                });
                if full_w <= avail {
                    ui.monospace(&full);
                } else {
                    ui.monospace(format!("{short}\u{2026}"))
                        .on_hover_text(full);
                }
            } else {
                ui.weak("Hover preview for pixel values");
            }
        } else {
            ui.weak("Hover preview for pixel values");
        }
        if workspace.tool == PreviewTool::Ruler {
            if let Some(measurement) = ruler_tool.measurement(input.nm_per_pixel) {
                ui.separator();
                ui.small(format!(
                    "{:.1} px | {:.2} µm",
                    measurement.pixel_distance, measurement.micrometers
                ));
            }
        }
    });

    popup_toggled
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
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
) -> bool {
    state.workspace.point_cloud.sanitize_controls();

    egui::Grid::new(egui::Id::new(input.viewer_id).with("pc_controls_grid"))
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Time range [ms]")
                .on_hover_text("How far back in time to show events");
            ui.add(
                egui::DragValue::new(&mut state.workspace.point_cloud.time_window_ms)
                    .speed(5.0)
                    .clamp_range(5.0..=2_000.0),
            );
            ui.end_row();

            ui.label("Max render")
                .on_hover_text("Limits rendered points for smoother interaction");
            ui.add(
                egui::DragValue::new(&mut state.workspace.point_cloud.point_limit)
                    .speed(250.0)
                    .clamp_range(1_000..=100_000),
            );
            ui.end_row();
        });

    let mut popup_toggled = false;
    ui.horizontal(|ui| {
        if ui.button("Reset Camera").clicked() {
            state.workspace.point_cloud.reset_camera();
        }
        if ui.button(input.popup_button_label).clicked() {
            popup_toggled = true;
        }
        ui.small("Drag to orbit. Scroll to zoom.");
    });

    popup_toggled
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

#[derive(Default)]
struct PreviewCanvasResult {
    roi_committed: bool,
    new_roi: Option<RoiConfig>,
}

struct PreviewCanvasOptions {
    scale_bar_settings: ScaleBarSettings,
    nm_per_pixel: f64,
    settings_locked: bool,
    max_height: f32,
}

fn draw_preview_canvas(
    ui: &mut egui::Ui,
    texture: &egui::TextureHandle,
    frame: &PreviewFrame,
    config: &CameraConfig,
    state: &mut ViewerState,
    options: PreviewCanvasOptions,
) -> ViewerOutput {
    let workspace = &mut state.workspace;
    let line_profile_tool = &mut state.line_profile_tool;
    let ruler_tool = &mut state.ruler_tool;
    let annotation_manager = &mut state.annotation_manager;
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

    let mut result = PreviewCanvasResult::default();
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
                    result.new_roi = Some(roi);
                    result.roi_committed = true;
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
            &scale_bar_settings,
            nm_per_pixel,
        );
    }

    ViewerOutput {
        roi_committed: result.roi_committed,
        new_roi: result.new_roi,
        ..Default::default()
    }
}

#[derive(Debug, Clone, Copy)]
struct PreviewViewport {
    base_sensor_rect: egui::Rect,
    visible_sensor_rect: egui::Rect,
    roi_rect: Option<egui::Rect>,
}

impl PreviewViewport {
    fn display_size(self, available_size: egui::Vec2) -> egui::Vec2 {
        let sensor_size = self.visible_sensor_rect.size().max(egui::vec2(1.0, 1.0));
        let scale = (available_size.x / sensor_size.x)
            .min(available_size.y / sensor_size.y)
            .max(1e-6);
        sensor_size * scale
    }

    fn uv_rect(self, frame: &PreviewFrame) -> egui::Rect {
        let frame_rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(frame.width.max(1) as f32, frame.height.max(1) as f32),
        );
        egui::Rect::from_min_max(
            egui::pos2(
                (self.visible_sensor_rect.min.x - frame_rect.min.x) / frame_rect.width(),
                (self.visible_sensor_rect.min.y - frame_rect.min.y) / frame_rect.height(),
            ),
            egui::pos2(
                (self.visible_sensor_rect.max.x - frame_rect.min.x) / frame_rect.width(),
                (self.visible_sensor_rect.max.y - frame_rect.min.y) / frame_rect.height(),
            ),
        )
    }

    fn sensor_to_screen(self, image_rect: egui::Rect, sensor_pos: egui::Pos2) -> egui::Pos2 {
        let rel_x = (sensor_pos.x - self.visible_sensor_rect.min.x)
            / self.visible_sensor_rect.width().max(1.0);
        let rel_y = (sensor_pos.y - self.visible_sensor_rect.min.y)
            / self.visible_sensor_rect.height().max(1.0);
        egui::pos2(
            image_rect.left() + rel_x * image_rect.width(),
            image_rect.top() + rel_y * image_rect.height(),
        )
    }

    fn screen_to_sensor(
        self,
        image_rect: egui::Rect,
        screen_pos: egui::Pos2,
    ) -> Option<(u16, u16)> {
        if !image_rect.contains(screen_pos) {
            return None;
        }
        let rel_x = ((screen_pos.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0);
        let rel_y = ((screen_pos.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0);
        let sensor_x = (self.visible_sensor_rect.min.x + rel_x * self.visible_sensor_rect.width())
            .floor()
            .max(self.base_sensor_rect.min.x)
            .min(self.base_sensor_rect.max.x - 1.0) as u16;
        let sensor_y = (self.visible_sensor_rect.min.y + rel_y * self.visible_sensor_rect.height())
            .floor()
            .max(self.base_sensor_rect.min.y)
            .min(self.base_sensor_rect.max.y - 1.0) as u16;
        Some((sensor_x, sensor_y))
    }
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

fn analysis_warning_color(severity: AnalysisSeverity, visuals: &egui::Visuals) -> egui::Color32 {
    match severity {
        AnalysisSeverity::Info => egui::Color32::from_rgb(30, 100, 220),
        AnalysisSeverity::Warning => visuals.warn_fg_color,
        AnalysisSeverity::Error => visuals.error_fg_color,
    }
}

fn draw_replay_transport(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    replay: ViewerReplayState,
    viewer_id: &str,
    output: &mut ViewerOutput,
) {
    if !replay.active || replay.duration_us == 0 {
        return;
    }

    let time_text = format!(
        "{} / {}",
        format_replay_time(replay.time_us),
        format_replay_time(replay.duration_us)
    );

    let mut new_speed: Option<f32> = None;

    ui.horizontal(|ui| {
        let play_pause = if replay.paused {
            "\u{25B6}"
        } else {
            "\u{23F8}"
        };
        if ui
            .add_enabled(!replay.finished, egui::Button::new(play_pause))
            .clicked()
        {
            output.replay_toggle_pause = true;
        }

        if ui.button("\u{23EE}").clicked() {
            output.replay_restart = true;
        }

        if ui.button("\u{23F9}").clicked() {
            output.replay_stop = true;
        }

        ui.separator();

        let selected_label = replay_speed_label(replay.speed);
        egui::ComboBox::from_id_source((viewer_id, "replay_speed_combo"))
            .selected_text(format!("Speed: {selected_label}"))
            .show_ui(ui, |ui| {
                for (speed, label) in REPLAY_SPEED_OPTIONS {
                    if ui
                        .selectable_label(replay_speed_matches(replay.speed, speed), label)
                        .clicked()
                    {
                        new_speed = Some(speed);
                    }
                }
            });

        ui.separator();

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

        let mut timeline_fraction = state.replay_seek_drag.unwrap_or(replay.fraction);
        let slider = egui::Slider::new(&mut timeline_fraction, 0.0..=1.0).show_value(false);
        let slider_width = (ui.available_width() - time_width).max(80.0);
        let response = ui.add_sized([slider_width, ui.spacing().interact_size.y], slider);
        if response.dragged() {
            state.replay_seek_drag = Some(timeline_fraction);
        }
        if response.drag_stopped() || (response.changed() && !response.dragged()) {
            state.replay_seek_drag = None;
            output.replay_seek_to = Some(timeline_fraction);
        }

        ui.label(&time_text);
        ui.separator();
        ui.small(format!(
            "{:.1} / {:.1} MB",
            replay.bytes_read as f64 / (1024.0 * 1024.0),
            replay.data_len as f64 / (1024.0 * 1024.0)
        ));
    });

    if let Some(speed) = new_speed {
        output.replay_set_speed = Some(speed);
    }
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
