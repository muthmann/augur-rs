use augur_core::{
    analysis::{AnalysisSeverity, AnalysisWarning, MarkerShape, Overlay},
    camera::DeviceInfo,
    config::{CameraConfig, RoiConfig},
    pipeline::{PipelineStatsSnapshot, PreviewFrame},
};
use egui_phosphor::regular as phosphor;

use crate::{
    app::PANEL_ROUNDING,
    colormap::Colormap,
    inspection_3d::Investigation3dState,
    investigation::{Investigation2dPoint, InvestigationState, StableRowKey},
    point_cloud::PointCloudState,
    preview::{reset_preview_render_cache, PreviewDisplaySettings, PreviewMode},
    preview_renderer::PreviewDisplayTexture,
    viewer_tools::{
        compute_scale_bar, AnnotationManager, AnnotationShape, AnnotationShapeKind, ContrastMode,
        ContrastSettings, HistogramWindow, LineProfileTool, RulerTool, ScaleBarPosition,
        ScaleBarSettings,
    },
};

pub(crate) const REPLAY_SPEED_OPTIONS: [(f32, &str); 6] = [
    (0.25, "0.25x"),
    (0.5, "0.5x"),
    (1.0, "1x"),
    (2.0, "2x"),
    (4.0, "4x"),
    (f32::INFINITY, "Max"),
];
const PREVIEW_ZOOM_MIN: f32 = 1.0;
pub(crate) const PREVIEW_ZOOM_MAX: f32 = 16.0;
const PREVIEW_CANVAS_MIN_HEIGHT: f32 = 180.0;
pub(crate) const DEFAULT_TIME_SURFACE_TAU_US: u64 = 30_000;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewCropTarget {
    None,
    HardwareRoi,
    Annotation(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AnnotationDragState {
    id: usize,
    last_sensor: (u16, u16),
}

#[derive(Debug, Clone)]
pub(crate) struct PreviewWorkspaceState {
    pub(crate) tool: PreviewTool,
    pub(crate) zoom: f32,
    pub(crate) pan: egui::Vec2,
    pub(crate) crop_target: PreviewCropTarget,
    pub(crate) hover_sensor: Option<(u16, u16)>,
    selection_anchor: Option<egui::Pos2>,
    pending_roi: Option<egui::Rect>,
    annotation_drag: Option<AnnotationDragState>,
    pub(crate) point_cloud: PointCloudState,
}

impl Default for PreviewWorkspaceState {
    fn default() -> Self {
        Self {
            tool: PreviewTool::None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            crop_target: PreviewCropTarget::None,
            hover_sensor: None,
            selection_anchor: None,
            pending_roi: None,
            annotation_drag: None,
            point_cloud: PointCloudState::default(),
        }
    }
}

impl PreviewWorkspaceState {
    pub(crate) fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.pending_roi = None;
        self.annotation_drag = None;
        self.tool = PreviewTool::None;
    }

    pub(crate) fn clear_session_state(&mut self) {
        self.hover_sensor = None;
        self.selection_anchor = None;
        self.pending_roi = None;
        self.annotation_drag = None;
        self.point_cloud.clear();
    }

    pub(crate) fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
    }

    pub(crate) fn crop_active(&self) -> bool {
        self.crop_target != PreviewCropTarget::None
    }

    pub(crate) fn toggle_crop_target(&mut self, selected_annotation: Option<usize>) {
        self.crop_target = if self.crop_active() {
            PreviewCropTarget::None
        } else if let Some(id) = selected_annotation {
            PreviewCropTarget::Annotation(id)
        } else {
            PreviewCropTarget::HardwareRoi
        };
    }

    pub(crate) fn clear_crop_target_if_annotation(&mut self, id: usize) {
        if self.crop_target == PreviewCropTarget::Annotation(id) {
            self.crop_target = PreviewCropTarget::None;
        }
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct ViewerOutput {
    pub(crate) roi_committed: bool,
    pub(crate) new_roi: Option<RoiConfig>,
    pub(crate) preview_mode_changed: bool,
    pub(crate) contrast_changed: bool,
    pub(crate) histogram_visibility_changed: bool,
    pub(crate) time_surface_tau_changed: bool,
    pub(crate) popup_toggled: bool,
    pub(crate) return_from_external: bool,
    pub(crate) mask_hotpixels_clicked: bool,
    pub(crate) probe_camera: bool,
    pub(crate) open_replay: bool,
    pub(crate) start_preview: bool,
    pub(crate) start_recording: bool,
    pub(crate) replay_toggle_pause: bool,
    pub(crate) replay_restart: bool,
    pub(crate) replay_stop: bool,
    pub(crate) replay_step_frames: Option<i64>,
    pub(crate) replay_seek_to: Option<f32>,
    pub(crate) replay_set_speed: Option<f32>,
    pub(crate) investigation_select_row: Option<StableRowKey>,
    pub(crate) investigation_hover_row: Option<StableRowKey>,
}

impl ViewerOutput {
    pub(crate) fn needs_preview_refresh(&self) -> bool {
        self.preview_mode_changed
            || self.contrast_changed
            || self.histogram_visibility_changed
            || self.time_surface_tau_changed
    }

    pub(crate) fn requests_root_update(&self) -> bool {
        self.popup_toggled
            || self.return_from_external
            || self.mask_hotpixels_clicked
            || self.probe_camera
            || self.open_replay
            || self.start_preview
            || self.start_recording
            || self.replay_toggle_pause
            || self.replay_restart
            || self.replay_stop
            || self.replay_step_frames.is_some()
            || self.replay_seek_to.is_some()
            || self.replay_set_speed.is_some()
            || self.investigation_select_row.is_some()
            || self.investigation_hover_row.is_some()
            || self.new_roi.is_some()
            || self.needs_preview_refresh()
    }

    pub(crate) fn has_replay_actions(&self) -> bool {
        self.replay_toggle_pause
            || self.replay_restart
            || self.replay_stop
            || self.replay_step_frames.is_some()
            || self.replay_seek_to.is_some()
            || self.replay_set_speed.is_some()
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.roi_committed |= other.roi_committed;
        if other.new_roi.is_some() {
            self.new_roi = other.new_roi;
        }
        self.preview_mode_changed |= other.preview_mode_changed;
        self.contrast_changed |= other.contrast_changed;
        self.histogram_visibility_changed |= other.histogram_visibility_changed;
        self.time_surface_tau_changed |= other.time_surface_tau_changed;
        self.popup_toggled |= other.popup_toggled;
        self.return_from_external |= other.return_from_external;
        self.mask_hotpixels_clicked |= other.mask_hotpixels_clicked;
        self.probe_camera |= other.probe_camera;
        self.open_replay |= other.open_replay;
        self.start_preview |= other.start_preview;
        self.start_recording |= other.start_recording;
        self.replay_toggle_pause |= other.replay_toggle_pause;
        self.replay_restart |= other.replay_restart;
        self.replay_stop |= other.replay_stop;
        if other.replay_step_frames.is_some() {
            self.replay_step_frames = other.replay_step_frames;
        }
        if other.replay_seek_to.is_some() {
            self.replay_seek_to = other.replay_seek_to;
        }
        if other.replay_set_speed.is_some() {
            self.replay_set_speed = other.replay_set_speed;
        }
        if other.investigation_select_row.is_some() {
            self.investigation_select_row = other.investigation_select_row;
        }
        if other.investigation_hover_row.is_some() {
            self.investigation_hover_row = other.investigation_hover_row;
        }
    }
}

#[derive(Debug)]
pub(crate) struct ViewerState {
    pub(crate) investigation: InvestigationState,
    pub(crate) investigation_3d: Investigation3dState,
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
            investigation: InvestigationState::default(),
            investigation_3d: Investigation3dState::default(),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewHistogramRequest {
    None,
    AutoContrast,
    Full,
}

impl ViewerState {
    pub(crate) fn clear_session_state(&mut self) {
        self.workspace.clear_session_state();
        self.replay_seek_drag = None;
    }

    pub(crate) fn preview_display_settings(&self) -> PreviewDisplaySettings {
        PreviewDisplaySettings {
            display_min: self.contrast_settings.display_min,
            display_max: self.contrast_settings.display_max,
            gamma: self.contrast_settings.gamma,
        }
    }

    pub(crate) fn apply_histogram(&mut self, histogram: Vec<u64>) {
        self.apply_auto_histogram(histogram.as_slice());
        self.histogram_window.set_histogram(histogram);
    }

    pub(crate) fn apply_auto_histogram(&mut self, histogram: &[u64]) {
        if self.contrast_settings.mode == ContrastMode::Auto {
            self.contrast_settings.update_auto_range(histogram);
        }
        self.clamp_histogram_range(histogram.len());
    }

    pub(crate) fn apply_auto_contrast_max(&mut self, display_max: u16) {
        if self.contrast_settings.mode == ContrastMode::Auto {
            self.contrast_settings.display_min = 0;
            self.contrast_settings.display_max = display_max.max(1);
        }
    }

    fn clamp_histogram_range(&mut self, histogram_len: usize) {
        let histogram_max = histogram_len.saturating_sub(1).min(u16::MAX as usize) as u16;
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
    }

    pub(crate) fn show_aux_windows(&mut self, ctx: &egui::Context) -> ViewerAuxChanges {
        let previous_contrast = self.contrast_settings.clone();
        let histogram_was_open = self.histogram_window.open;
        self.histogram_window
            .show(ctx, &mut self.contrast_settings, self.preview_mode);
        let contrast_changed = self.contrast_settings != previous_contrast;
        self.line_profile_tool.show_window(ctx);
        ViewerAuxChanges {
            contrast_changed,
            histogram_visibility_changed: self.histogram_window.open != histogram_was_open,
        }
    }

    pub(crate) fn preview_histogram_request(&self) -> PreviewHistogramRequest {
        if self.histogram_window.open {
            PreviewHistogramRequest::Full
        } else if self.contrast_settings.mode == ContrastMode::Auto {
            PreviewHistogramRequest::AutoContrast
        } else {
            PreviewHistogramRequest::None
        }
    }

    pub(crate) fn needs_line_profile_refresh(&self) -> bool {
        self.line_profile_tool.has_line()
            && (self.line_profile_tool.window_open
                || self.workspace.tool == PreviewTool::LineProfile)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewerAuxChanges {
    pub(crate) contrast_changed: bool,
    pub(crate) histogram_visibility_changed: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ViewerReplayState {
    pub(crate) active: bool,
    pub(crate) paused: bool,
    pub(crate) finished: bool,
    pub(crate) stepping: bool,
    pub(crate) speed: f32,
    pub(crate) fraction: f32,
    pub(crate) duration_us: u64,
    pub(crate) time_us: u64,
    pub(crate) bytes_read: u64,
    pub(crate) data_len: u64,
}

pub(crate) struct ViewerInput<'a> {
    pub(crate) texture: Option<&'a PreviewDisplayTexture>,
    pub(crate) frame: Option<&'a PreviewFrame>,
    pub(crate) time_surface_hover_value: Option<u8>,
    pub(crate) overlays: &'a [Overlay],
    pub(crate) camera_info: Option<&'a DeviceInfo>,
    pub(crate) nm_per_pixel: f64,
    pub(crate) config: &'a CameraConfig,
    pub(crate) investigation_points_2d: &'a [Investigation2dPoint],
    pub(crate) selected_row: Option<&'a StableRowKey>,
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
    pub(crate) popup_button_tooltip: &'a str,
    pub(crate) viewer_id: &'a str,
}

pub(crate) fn draw_viewer(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: ViewerInput<'_>,
) -> ViewerOutput {
    let mut output = ViewerOutput::default();
    output.merge(draw_viewer_top_chrome(ctx, ui, state, &input));

    let max_image_height = (ui.available_height()
        - viewer_bottom_chrome_reserve(state, input.replay))
    .max(PREVIEW_CANVAS_MIN_HEIGHT);
    output.merge(draw_viewer_canvas(ui, state, &input, max_image_height));
    output.merge(draw_viewer_bottom_chrome(ui, state, &input));

    output
}

pub(crate) fn draw_viewer_top_chrome(
    ctx: &egui::Context,
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
) -> ViewerOutput {
    let mut output = ViewerOutput::default();
    if input.mode == AppMode::Replaying {
        handle_replay_shortcuts(ctx, input.replay, &mut output);
    }

    // Viewer head row: compact title + bus / resolution / firmware meta,
    // matching the design's `.viewer-head` strip
    // (`Viewer  EVK3 / IMX636  1280 × 720  firmware 3.2.3`).
    let palette = crate::theme::palette_for_visuals(ui.visuals());
    let head_height = ui.spacing().interact_size.y.max(20.0);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), head_height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = crate::theme::sp::SP_2;
            ui.label(
                egui::RichText::new(preview_heading(input.mode))
                    .size(13.0)
                    .strong()
                    .color(palette.ink),
            );
            if input.mode == AppMode::Replaying {
                ui.label(
                    egui::RichText::new("\u{2014}")
                        .size(11.0)
                        .color(palette.fg_3),
                );
            }
            if let Some(info) = input.camera_info {
                let bus = info.compatible.as_deref().unwrap_or(&info.model);
                let firmware = info.firmware.as_deref().unwrap_or("\u{2014}");
                let resolution = format!(
                    "{} \u{00D7} {}",
                    input.config.roi.width, input.config.roi.height
                );
                ui.label(
                    egui::RichText::new(format!(
                        "{bus}  \u{00B7}  {resolution}  \u{00B7}  firmware {firmware}"
                    ))
                    .monospace()
                    .size(11.0)
                    .color(palette.fg_2),
                );
            }
            // Right-aligned hint row: keep the primary layout shortcuts visible
            // in the viewer chrome where the prototype teaches them.
            // Use a nested right-to-left layout filling the remaining width
            // so the hint truly anchors to the right edge regardless of the
            // header's measured size.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("1 2D   2 Split   3 3D")
                        .size(11.0)
                        .color(palette.fg_3),
                );
            });
        },
    );
    ui.separator();
    output.merge(draw_preview_toolbar(ui, state, input.popup_active, input));

    output
}

pub(crate) fn viewer_bottom_chrome_reserve(_state: &ViewerState, replay: ViewerReplayState) -> f32 {
    56.0 + 42.0 + replay_transport_reserve(replay)
}

pub(crate) fn draw_viewer_canvas(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
    max_image_height: f32,
) -> ViewerOutput {
    let mut output = ViewerOutput::default();
    if input.external_streaming {
        draw_text_placeholder(ui, max_image_height, input.external_streaming_label);
        ui.add_space(8.0);
        if ui.button("Return to augur").clicked() {
            output.return_from_external = true;
        }
    } else if let (Some(texture), Some(frame)) = (input.texture, input.frame) {
        let scale_bar_settings = state.scale_bar_settings.clone();
        output.merge(draw_preview_canvas(
            ui,
            texture,
            frame,
            input.overlays,
            input.config,
            state,
            PreviewCanvasOptions {
                scale_bar_settings,
                nm_per_pixel: input.nm_per_pixel,
                settings_locked: input.settings_locked,
                auto_focus_roi: input.mode == AppMode::Replaying || input.settings_locked,
                investigation_points: input.investigation_points_2d,
                selected_row: input.selected_row,
                max_height: max_image_height,
            },
        ));
    } else {
        state.workspace.hover_sensor = None;
        output.merge(draw_empty_preview_placeholder(
            ui,
            max_image_height,
            input.mode,
            input.camera_info.is_some(),
        ));
    }
    output
}

pub(crate) fn draw_viewer_bottom_chrome(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
) -> ViewerOutput {
    let mut output = ViewerOutput::default();
    draw_display_strip(ui, state, input, &mut output, 0.0);
    draw_replay_transport(ui, state, input.replay, input.viewer_id, &mut output);
    ui.add_space(2.0);
    draw_status_footer(ui, state, input, &mut output, 56.0);
    output
}

pub(crate) fn draw_text_placeholder(ui: &mut egui::Ui, max_image_height: f32, message: &str) {
    let placeholder_height = max_image_height;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width().min(ui.clip_rect().width()).max(1.0),
            placeholder_height,
        ),
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

fn replay_transport_reserve(replay: ViewerReplayState) -> f32 {
    if replay.active {
        34.0
    } else {
        0.0
    }
}

fn draw_display_strip(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
    output: &mut ViewerOutput,
    _max_height: f32,
) {
    ui.push_id((input.viewer_id, "display_strip"), |ui| {
        let palette = crate::theme::palette_for_visuals(ui.visuals());
        let frame = egui::Frame::none()
            .fill(palette.bg_1)
            .inner_margin(egui::Margin::symmetric(10.0, 4.0))
            .stroke(egui::Stroke::new(1.0, palette.line));
        frame.show(ui, |ui| {
            egui::ScrollArea::horizontal()
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                    // Group 1 — preview mode
                    ui.label(egui::RichText::new("Mode").size(11.0).strong());
                    let previous_preview_mode = state.preview_mode;
                    egui::ComboBox::from_id_source((input.viewer_id, "preview_mode"))
                        .width(140.0)
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
                    output.preview_mode_changed |=
                        state.preview_mode != previous_preview_mode;
                    if output.preview_mode_changed {
                        reset_preview_render_cache();
                    }

                    crate::theme::toolbar_separator(ui);

                    // Group 2 — scale bar
                    ui.checkbox(&mut state.scale_bar_settings.show, "Scale bar");
                    if state.scale_bar_settings.show {
                        egui::ComboBox::from_id_source((
                            input.viewer_id,
                            "scale_bar_position",
                        ))
                        .width(100.0)
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

                            crate::theme::toolbar_separator(ui);
                            let mut tau_ms = state.time_surface_tau_us as f64 / 1_000.0;
                            ui.label(
                                egui::RichText::new("Decay \u{03C4} [ms]")
                                    .size(11.0)
                                    .strong(),
                            );
                            let response = ui.add(
                                egui::Slider::new(&mut tau_ms, 1.0..=1_000.0)
                                    .logarithmic(true)
                                    .show_value(true),
                            );
                            if response.changed() {
                                state.time_surface_tau_us =
                                    (tau_ms * 1_000.0).round().max(1.0) as u64;
                                output.time_surface_tau_changed = true;
                            }
                            if matches!(state.preview_mode, PreviewMode::TimeSurface)
                                && input
                                    .frame
                                    .is_some_and(|frame| !frame.raw_events_available())
                            {
                                ui.small(
                                    "Time Surface needs raw preview events. Augur will fall back to grayscale intensity until a raw-event frame is available.",
                                );
                            }

                            crate::theme::toolbar_separator(ui);
                            let ann_count = state.annotation_manager.annotations().len();
                            ui.label(
                                egui::RichText::new(format!(
                                    "Annotations: {} shape{}",
                                    ann_count,
                                    if ann_count == 1 { "" } else { "s" }
                                ))
                                .monospace()
                                .size(11.0)
                                .color(palette.fg_2),
                            );
                    });
                });
        });
    });
}

fn draw_status_footer(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    input: &ViewerInput<'_>,
    output: &mut ViewerOutput,
    max_height: f32,
) {
    let emphasize_details = input.config_dirty
        || input.acq_dirty
        || !input.analysis_warnings.is_empty()
        || input.replay_open_task_active
        || input.last_error.is_some();

    ui.push_id((input.viewer_id, "status_footer"), |ui| {
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .auto_shrink([true, true])
            .show(ui, |ui| {
                crate::theme::constrain_section_width(ui);
                // Single `·`-separated diagnostics line under the transport,
                // matching the design's
                // `24.8 Mev/s · 9.1 MB/s · 00:01:23 elapsed · ON 54.3% OFF 45.7% · 1,192,227 ev/frame · Diagnostics ▾`.
                // Hover readout / ruler measurement chip live on the same row
                // when the user is interacting with the canvas.
                draw_unified_diagnostics_row(
                    ui,
                    input.pipeline_stats,
                    preview_polarity_split(input.frame).as_ref(),
                );
                if let Some(hover_summary) = preview_hover_status_text(state, input) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        ui.label(
                            egui::RichText::new(hover_summary)
                                .monospace()
                                .size(11.0)
                                .color(crate::theme::palette_for_visuals(ui.visuals()).fg_2),
                        );
                    });
                }
                if state.workspace.tool == PreviewTool::Ruler {
                    if let Some(measurement) =
                        state.ruler_tool.measurement(input.nm_per_pixel)
                    {
                        ui.small(format!(
                            "{:.1} px  \u{00B7}  {:.2} \u{00B5}m",
                            measurement.pixel_distance, measurement.micrometers
                        ));
                    }
                }

                egui::CollapsingHeader::new("Diagnostics")
                    .id_source((input.viewer_id, "pipeline_details"))
                    .default_open(emphasize_details)
                    .show(ui, |ui| {
                        crate::theme::constrain_section_width(ui);
                        let mut rendered_anything = false;

                        if let Some(stats) = input.pipeline_stats {
                            rendered_anything = true;
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
                            ui.small(format!(
                                "Preview thread avg [ms]: decode {:.3}  |  accumulate {:.3}  |  frame send {:.3}",
                                stats.preview_decode_avg_ms(),
                                stats.preview_accumulate_avg_ms(),
                                stats.preview_frame_send_avg_ms(),
                            ));
                            // Recording integrity: preview drops are expected
                            // and harmless, but a stalled recording path means
                            // the camera FIFO overflowed and events are gone.
                            if stats.recording_may_have_gaps() {
                                ui.colored_label(
                                    ui.visuals().error_fg_color,
                                    format!(
                                        "Recording path stalled {}\u{00D7} (longest {:.1} ms, total {:.1} ms) \u{2014} the recording may have event gaps.",
                                        stats.raw_pool_starvation_events,
                                        stats.raw_pool_starvation_max_us as f64 / 1_000.0,
                                        stats.raw_pool_starvation_us as f64 / 1_000.0,
                                    ),
                                );
                            } else {
                                ui.small("Recording path: no stalls, no host-side event loss.");
                            }
                        }

                        if let Some(frame_summary) = preview_frame_status_summary(input.frame) {
                            rendered_anything = true;
                            ui.small(frame_summary);
                        }

                        if input.mode != AppMode::Idle && input.mode != AppMode::Replaying {
                            rendered_anything = true;
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
                            rendered_anything = true;
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
                                    .add_enabled(
                                        can_copy,
                                        egui::Button::new("Mask detected hotpixels"),
                                    )
                                    .clicked()
                                {
                                    output.mask_hotpixels_clicked = true;
                                }
                                if !can_copy {
                                    ui.label(
                                        "Unlock runtime settings to copy detections into the DEM mask.",
                                    );
                                }
                            }
                        }

                        if let Some(notice) = input.analysis_notice {
                            rendered_anything = true;
                            ui.label(notice);
                        }

                        if input.replay_open_task_active {
                            rendered_anything = true;
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(input.replay_notice.unwrap_or("Opening replay..."));
                            });
                        }

                        if let Some(err) = input.last_error {
                            rendered_anything = true;
                            ui.separator();
                            ui.colored_label(ui.visuals().error_fg_color, err);
                        }

                        if !rendered_anything {
                            ui.small("No additional diagnostics.");
                        }
                    });
            });
    });
}

/// Single-row, `·`-separated diagnostics line that runs beneath the
/// transport scrubber. Matches the design's
/// `24.8 Mev/s · 9.1 MB/s · 00:01:23 elapsed · ON 54.3% OFF 45.7% · 1,192,227 ev/frame · Diagnostics ▾`.
/// The trailing `Diagnostics ▾` chevron is provided by the
/// `CollapsingHeader` rendered just below this row.
fn draw_unified_diagnostics_row(
    ui: &mut egui::Ui,
    stats: Option<&augur_core::pipeline::PipelineStatsSnapshot>,
    polarity: Option<&PolaritySplit>,
) {
    let palette = crate::theme::palette_for_visuals(ui.visuals());
    let bullet = || {
        egui::RichText::new("\u{00B7}")
            .size(11.0)
            .color(palette.fg_4)
    };
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let mut wrote_left = false;
        if let Some(stats) = stats {
            let elapsed = stats.elapsed_s as u64;
            let elapsed_str = format!(
                "{:02}:{:02}:{:02}",
                elapsed / 3600,
                (elapsed % 3600) / 60,
                elapsed % 60
            );
            ui.label(
                egui::RichText::new(format!("\u{25A1} {:.2} Mev/s", stats.mev_per_s))
                    .monospace()
                    .size(11.0)
                    .color(palette.fg_1),
            );
            ui.label(bullet());
            ui.label(
                egui::RichText::new(format!("\u{2191} {:.2} MB/s", stats.mb_per_s))
                    .monospace()
                    .size(11.0)
                    .color(palette.fg_1),
            );
            ui.label(bullet());
            ui.label(
                egui::RichText::new(format!("\u{25EF} {elapsed_str} elapsed"))
                    .monospace()
                    .size(11.0)
                    .color(palette.fg_1),
            );
            wrote_left = true;
        }
        if let Some(split) = polarity {
            if wrote_left {
                ui.label(bullet());
            }
            let on_color = egui::Color32::from_rgb(
                crate::theme::POLARITY_ON_RGB[0],
                crate::theme::POLARITY_ON_RGB[1],
                crate::theme::POLARITY_ON_RGB[2],
            );
            let off_color = egui::Color32::from_rgb(
                crate::theme::POLARITY_OFF_RGB[0],
                crate::theme::POLARITY_OFF_RGB[1],
                crate::theme::POLARITY_OFF_RGB[2],
            );
            ui.label(
                egui::RichText::new(format!("ON {:.1}%", split.on_pct))
                    .monospace()
                    .size(11.0)
                    .color(on_color),
            );
            ui.label(
                egui::RichText::new(format!("OFF {:.1}%", split.off_pct))
                    .monospace()
                    .size(11.0)
                    .color(off_color),
            );
            ui.label(bullet());
            ui.label(
                egui::RichText::new(format_thousands(split.total))
                    .monospace()
                    .size(11.0)
                    .color(palette.fg_1),
            );
            ui.label(
                egui::RichText::new("ev/frame")
                    .size(11.0)
                    .color(palette.fg_3),
            );
        } else if !wrote_left {
            ui.label(
                egui::RichText::new("No live pipeline.")
                    .size(11.0)
                    .color(palette.fg_3),
            );
        }
    });
}

/// Format a `u64` with `,` thousands separators — e.g. `1192227 → 1,192,227`.
/// Matches the design's `1,192,227 ev/frame` rendering.
fn format_thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// ON / OFF event-count percentage split for a preview frame. Mirrors the
/// design's coloured `ON 54.3%` / `OFF 45.7%` chips.
struct PolaritySplit {
    on_pct: f64,
    off_pct: f64,
    total: u64,
}

fn preview_polarity_split(frame: Option<&PreviewFrame>) -> Option<PolaritySplit> {
    let frame = frame?;
    let total = frame.on_count + frame.off_count;
    if total == 0 {
        return None;
    }
    Some(PolaritySplit {
        on_pct: frame.on_count as f64 * 100.0 / total as f64,
        off_pct: frame.off_count as f64 * 100.0 / total as f64,
        total,
    })
}

fn preview_frame_status_summary(frame: Option<&PreviewFrame>) -> Option<String> {
    let split = preview_polarity_split(frame)?;
    Some(format!(
        "ON {:.1}% / OFF {:.1}% ({} ev this frame)",
        split.on_pct, split.off_pct, split.total
    ))
}

fn preview_hover_status_text(state: &ViewerState, input: &ViewerInput<'_>) -> Option<String> {
    let (x, y) = state.workspace.hover_sensor?;
    let frame = input.frame?;
    let width = usize::from(frame.width.max(1));
    let idx = usize::from(y) * width + usize::from(x);
    if idx >= frame.pixels.len() {
        return None;
    }

    Some(match state.preview_mode {
        PreviewMode::TimeSurface => {
            let value = input
                .time_surface_hover_value
                .map(|value| value.to_string())
                .unwrap_or_else(|| "n/a".to_owned());
            format!("x {x}, y {y} | Value: {value} Total: {}", frame.pixels[idx])
        }
        _ => format!(
            "x {x}, y {y} | ON: {} OFF: {} Total: {}",
            frame.pixels_on[idx], frame.pixels_off[idx], frame.pixels[idx]
        ),
    })
}

fn draw_preview_toolbar(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    popup_active: bool,
    input: &ViewerInput<'_>,
) -> ViewerOutput {
    let selected_annotation = state.annotation_manager.selected_id();
    let workspace = &mut state.workspace;
    let line_profile_tool = &mut state.line_profile_tool;
    let ruler_tool = &mut state.ruler_tool;
    let histogram_open = &mut state.histogram_window.open;

    let mut output = ViewerOutput::default();
    let palette = crate::theme::palette_for_visuals(ui.visuals());
    let toolbar_height = ui.spacing().interact_size.y + ui.spacing().item_spacing.y + 8.0;
    let (toolbar_rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), toolbar_height),
        egui::Sense::hover(),
    );
    ui.allocate_ui_at_rect(toolbar_rect, |ui| {
        egui::ScrollArea::horizontal()
            .id_source((input.viewer_id, "preview_toolbar_scroll"))
            .auto_shrink([false, false])
            .max_height(toolbar_height)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                // Group 1 — selection & annotation tools
                if crate::theme::icon_toggle_button(
                    ui,
                    workspace.tool == PreviewTool::None,
                    phosphor::CURSOR,
                    "Pointer",
                )
                .clicked()
                {
                    workspace.clear_selection();
                    line_profile_tool.clear();
                    ruler_tool.clear();
                    state.annotation_manager.cancel_drawing();
                }

                let roi_tooltip = if input.settings_locked {
                    "Hardware ROI editing is disabled during replay. Use the rectangle annotation tool instead."
                } else {
                    "Select hardware ROI"
                };
                let roi_response = crate::theme::icon_toggle_button(
                    ui,
                    workspace.tool == PreviewTool::SelectRoi,
                    phosphor::SELECTION,
                    roi_tooltip,
                );
                let roi_response = if input.settings_locked {
                    roi_response.on_hover_text(roi_tooltip)
                } else {
                    roi_response
                };
                if roi_response.clicked() {
                    if workspace.tool == PreviewTool::SelectRoi {
                        workspace.clear_selection();
                    } else {
                        workspace.tool = PreviewTool::SelectRoi;
                        workspace.selection_anchor = None;
                        workspace.pending_roi = None;
                        workspace.annotation_drag = None;
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

                crate::theme::toolbar_separator(ui);

                // Group 2 — zoom & crop
                if crate::theme::icon_toggle_button(
                    ui,
                    false,
                    phosphor::MAGNIFYING_GLASS_MINUS,
                    "Zoom out",
                )
                .clicked()
                {
                    workspace.zoom = (workspace.zoom / 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
                    if (workspace.zoom - PREVIEW_ZOOM_MIN).abs() < f32::EPSILON {
                        workspace.pan = egui::Vec2::ZERO;
                    }
                }
                if crate::theme::icon_toggle_button(
                    ui,
                    false,
                    phosphor::MAGNIFYING_GLASS_PLUS,
                    "Zoom in",
                )
                .clicked()
                {
                    workspace.zoom = (workspace.zoom * 1.25).clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);
                }
                if crate::theme::icon_toggle_button(
                    ui,
                    false,
                    phosphor::FRAME_CORNERS,
                    "Fit to window",
                )
                .clicked()
                {
                    workspace.reset_zoom();
                }
                if crate::theme::icon_toggle_button(
                    ui,
                    workspace.crop_active(),
                    phosphor::CROP,
                    "Crop to ROI",
                )
                .clicked()
                {
                    workspace.toggle_crop_target(selected_annotation);
                }

                crate::theme::toolbar_separator(ui);

                // Group 3 — histogram & popout
                if crate::theme::icon_toggle_button(
                    ui,
                    *histogram_open,
                    phosphor::CHART_BAR,
                    "Histogram & Brightness/Contrast",
                )
                .clicked()
                {
                    *histogram_open = !*histogram_open;
                    output.histogram_visibility_changed = true;
                }
                if crate::theme::icon_toggle_button(
                    ui,
                    popup_active,
                    phosphor::ARROW_SQUARE_OUT,
                    input.popup_button_tooltip,
                )
                .clicked()
                {
                    output.popup_toggled = true;
                }

                });
            });
    });
    ui.painter_at(toolbar_rect).text(
        toolbar_rect.right_center() - egui::vec2(crate::theme::sp::SP_3, 0.0),
        egui::Align2::RIGHT_CENTER,
        "Hover for pixel values  \u{00B7}  Esc clears tool",
        egui::FontId::proportional(10.0),
        palette.fg_3,
    );

    output
}

fn preview_tool_button(
    ui: &mut egui::Ui,
    workspace: &mut PreviewWorkspaceState,
    tool: PreviewTool,
    icon: &str,
    tooltip: &str,
) -> bool {
    if crate::theme::icon_toggle_button(ui, workspace.tool == tool, icon, tooltip).clicked() {
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

fn draw_empty_preview_placeholder(
    ui: &mut egui::Ui,
    max_image_height: f32,
    mode: AppMode,
    camera_detected: bool,
) -> ViewerOutput {
    let mut output = ViewerOutput::default();
    if mode != AppMode::Idle {
        draw_text_placeholder(
            ui,
            max_image_height,
            empty_preview_message(mode, camera_detected),
        );
        return output;
    }

    let available = egui::vec2(ui.available_width(), max_image_height);
    ui.allocate_ui_with_layout(
        available,
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.add_space((available.y - 150.0).max(24.0) * 0.5);
            ui.heading(if camera_detected {
                "Camera detected"
            } else {
                "No camera detected"
            });
            ui.add_space(6.0);
            ui.label(if camera_detected {
                "Start a live preview or begin recording."
            } else {
                "Probe the camera here, or open an existing recording."
            });
            ui.add_space(14.0);
            ui.horizontal_centered(|ui| {
                if camera_detected {
                    if ui
                        .add(crate::theme::primary_button("Start Preview"))
                        .clicked()
                    {
                        output.start_preview = true;
                    }
                    if ui.button("Record").clicked() {
                        output.start_recording = true;
                    }
                } else if ui
                    .add(crate::theme::primary_button("Probe Camera"))
                    .clicked()
                {
                    output.probe_camera = true;
                }
                if ui.button("Open Replay…").clicked() {
                    output.open_replay = true;
                }
            });
        },
    );
    output
}

#[derive(Default)]
struct PreviewCanvasResult {
    roi_committed: bool,
    new_roi: Option<RoiConfig>,
}

#[derive(Debug, Clone)]
struct OverlayPickCandidate {
    sensor_position: egui::Pos2,
    item_key: Option<StableRowKey>,
}

struct PreviewCanvasOptions<'a> {
    scale_bar_settings: ScaleBarSettings,
    nm_per_pixel: f64,
    settings_locked: bool,
    auto_focus_roi: bool,
    investigation_points: &'a [Investigation2dPoint],
    selected_row: Option<&'a StableRowKey>,
    max_height: f32,
}

fn draw_preview_canvas(
    ui: &mut egui::Ui,
    texture: &PreviewDisplayTexture,
    frame: &PreviewFrame,
    overlays: &[Overlay],
    config: &CameraConfig,
    state: &mut ViewerState,
    options: PreviewCanvasOptions<'_>,
) -> ViewerOutput {
    let workspace = &mut state.workspace;
    let line_profile_tool = &mut state.line_profile_tool;
    let ruler_tool = &mut state.ruler_tool;
    let annotation_manager = &mut state.annotation_manager;
    let PreviewCanvasOptions {
        scale_bar_settings,
        nm_per_pixel,
        settings_locked,
        auto_focus_roi,
        investigation_points,
        selected_row,
        max_height,
    } = options;

    let viewport = build_preview_viewport_with_options(
        frame,
        config,
        annotation_manager,
        workspace,
        auto_focus_roi,
    );
    let available_height = ui.available_height().min(ui.clip_rect().height());
    let canvas_w = ui.available_width().min(ui.clip_rect().width()).max(1.0);
    let canvas_h = max_height
        .min(available_height.max(PREVIEW_CANVAS_MIN_HEIGHT))
        .max(1.0);
    // Size the canvas to the image's natural aspect ratio — no black letterbox bars.
    let display_size = viewport.display_size(egui::vec2(canvas_w, canvas_h));
    let canvas_size = egui::vec2(
        display_size.x.max(1.0),
        display_size.y.max(PREVIEW_CANVAS_MIN_HEIGHT),
    );
    let (canvas_rect, response) =
        ui.allocate_exact_size(canvas_size, egui::Sense::click_and_drag());
    let image_rect = canvas_rect;
    ui.painter()
        .rect_filled(canvas_rect, 4.0, crate::theme::CANVAS_BG);
    texture.paint_at(ui, image_rect, viewport.uv_rect(frame));

    workspace.hover_sensor = response
        .hover_pos()
        .and_then(|pos| viewport.screen_to_sensor(image_rect, pos));
    let pointer_sensor = response
        .interact_pointer_pos()
        .and_then(|pos| viewport.screen_to_sensor(image_rect, pos));
    let hovered_overlay = workspace
        .hover_sensor
        .and_then(|sensor| pick_overlay_candidate(overlays, sensor));
    let hovered_overlay_row = hovered_overlay
        .as_ref()
        .and_then(|candidate| overlay_candidate_row(candidate, investigation_points));
    let hovered_investigation = workspace
        .hover_sensor
        .and_then(|sensor| pick_investigation_point(investigation_points, sensor));
    let hovered_row = hovered_overlay_row
        .clone()
        .or_else(|| hovered_investigation.map(|point| point.item_key.clone()));

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
                    if workspace.crop_target == PreviewCropTarget::HardwareRoi {
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
    } else {
        if response.drag_started() {
            if let Some(pointer_pos) = pointer_sensor {
                if let Some(annotation_id) = annotation_manager.annotation_id_at(pointer_pos) {
                    annotation_manager.select(annotation_id);
                    workspace.annotation_drag = Some(AnnotationDragState {
                        id: annotation_id,
                        last_sensor: pointer_pos,
                    });
                } else {
                    annotation_manager.clear_selection();
                    workspace.annotation_drag = None;
                }
            }
        }
        if response.dragged() {
            if let (Some(drag), Some(pointer_pos)) = (workspace.annotation_drag, pointer_sensor) {
                let dx = i32::from(pointer_pos.0) - i32::from(drag.last_sensor.0);
                let dy = i32::from(pointer_pos.1) - i32::from(drag.last_sensor.1);
                if annotation_manager.translate_annotation(
                    drag.id,
                    dx,
                    dy,
                    frame.width,
                    frame.height,
                ) {
                    workspace.annotation_drag = Some(AnnotationDragState {
                        id: drag.id,
                        last_sensor: pointer_pos,
                    });
                }
            } else if workspace.zoom > PREVIEW_ZOOM_MIN {
                let delta = ui.ctx().input(|input| input.pointer.delta());
                workspace.pan += egui::vec2(
                    -delta.x * viewport.visible_sensor_rect.width() / image_rect.width().max(1.0),
                    -delta.y * viewport.visible_sensor_rect.height() / image_rect.height().max(1.0),
                );
            }
        }
        if response.drag_stopped() {
            workspace.annotation_drag = None;
        }
        if response.clicked() {
            if let Some(pointer_pos) = pointer_sensor {
                if !annotation_manager.select_at(pointer_pos) {
                    annotation_manager.clear_selection();
                    if let Some(point) = pick_investigation_point(investigation_points, pointer_pos)
                    {
                        let output = ViewerOutput {
                            roi_committed: result.roi_committed,
                            new_roi: result.new_roi,
                            investigation_select_row: Some(point.item_key.clone()),
                            investigation_hover_row: hovered_investigation
                                .map(|point| point.item_key.clone()),
                            ..Default::default()
                        };
                        return output;
                    }
                    if let Some(candidate) = pick_overlay_candidate(overlays, pointer_pos) {
                        if let Some(item_key) =
                            overlay_candidate_row(&candidate, investigation_points)
                        {
                            let output = ViewerOutput {
                                roi_committed: result.roi_committed,
                                new_roi: result.new_roi,
                                investigation_select_row: Some(item_key),
                                investigation_hover_row: hovered_row,
                                ..Default::default()
                            };
                            return output;
                        }
                    }
                }
            } else {
                annotation_manager.clear_selection();
            }
        }
    }

    let painter = ui
        .painter()
        .with_clip_rect(image_rect.intersect(ui.clip_rect()));
    paint_analysis_overlays(&painter, image_rect, viewport, overlays);
    paint_investigation_points(
        &painter,
        image_rect,
        viewport,
        investigation_points,
        selected_row,
        hovered_row.as_ref(),
    );
    if let Some(current_roi) = viewport.roi_rect {
        if !workspace.crop_active() {
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

    // Glass-morphism preview readout overlay anchored to the canvas
    // (bottom-left). Shows the current hover x/y and the per-pixel ON/OFF
    // counts when available — matches the design's `.preview-readout` box.
    if let Some((sx, sy)) = workspace.hover_sensor {
        let width = usize::from(frame.width.max(1));
        let idx = usize::from(sy) * width + usize::from(sx);
        let total = frame.pixels.get(idx).copied();
        let on = frame.pixels_on.get(idx).copied();
        let off = frame.pixels_off.get(idx).copied();
        let mut parts = vec![format!("x {sx} \u{00B7} y {sy}")];
        if let (Some(on), Some(off)) = (on, off) {
            parts.push(format!("ON {on}"));
            parts.push(format!("OFF {off}"));
        }
        if let Some(total) = total {
            parts.push(format!("total {total}"));
        }
        let text = parts.join("   ");
        let pad = egui::vec2(8.0, 4.0);
        let font = egui::FontId::monospace(11.0);
        let galley = ui.fonts(|f| {
            f.layout_no_wrap(
                text,
                font.clone(),
                egui::Color32::from_rgb(0xe9, 0xeb, 0xef),
            )
        });
        let box_size = egui::vec2(galley.size().x + pad.x * 2.0, galley.size().y + pad.y * 2.0);
        let box_rect = egui::Rect::from_min_size(
            egui::pos2(
                image_rect.left() + 8.0,
                image_rect.bottom() - box_size.y - 8.0,
            ),
            box_size,
        );
        let painter = ui.painter_at(image_rect.intersect(ui.clip_rect()));
        painter.rect_filled(box_rect, 4.0, egui::Color32::from_black_alpha(180));
        painter.galley(
            egui::pos2(box_rect.left() + pad.x, box_rect.top() + pad.y),
            galley,
            egui::Color32::from_rgb(0xe9, 0xeb, 0xef),
        );
    }

    ViewerOutput {
        roi_committed: result.roi_committed,
        new_roi: result.new_roi,
        investigation_hover_row: hovered_row,
        ..Default::default()
    }
}

fn pick_overlay_candidate(
    overlays: &[Overlay],
    sensor: (u16, u16),
) -> Option<OverlayPickCandidate> {
    let sensor = egui::pos2(f32::from(sensor.0), f32::from(sensor.1));
    let mut best_keyed: Option<(OverlayPickCandidate, f32)> = None;
    let mut best_unkeyed: Option<(OverlayPickCandidate, f32)> = None;

    let update_best = |best: &mut Option<(OverlayPickCandidate, f32)>,
                       sensor_position: egui::Pos2,
                       distance: f32,
                       item_key: Option<StableRowKey>| {
        match best {
            Some((_, best_distance)) if distance >= *best_distance => {}
            _ => {
                *best = Some((
                    OverlayPickCandidate {
                        sensor_position,
                        item_key,
                    },
                    distance,
                ));
            }
        }
    };

    for overlay in overlays {
        match overlay {
            Overlay::HighlightPixels { pixels, .. } => {
                for pixel in pixels {
                    let sensor_position =
                        egui::pos2(f32::from(pixel.x) + 0.5, f32::from(pixel.y) + 0.5);
                    let distance = sensor_position.distance(sensor);
                    if distance <= 1.1 {
                        update_best(&mut best_unkeyed, sensor_position, distance, None);
                    }
                }
            }
            Overlay::CrosshairMarkers {
                markers, arm_len, ..
            } => {
                let max_distance = f32::from((*arm_len).max(6)) + 2.0;
                for marker in markers {
                    let sensor_position = egui::pos2(marker.x, marker.y);
                    let distance = sensor_position.distance(sensor);
                    if distance <= max_distance {
                        update_best(&mut best_unkeyed, sensor_position, distance, None);
                    }
                }
            }
            Overlay::MarkerOverlay {
                markers,
                dataset_id,
                ..
            } => {
                for marker in markers {
                    let sensor_position = egui::pos2(marker.x, marker.y);
                    let max_distance = marker.size.max(5.0);
                    let distance = sensor_position.distance(sensor);
                    if distance > max_distance {
                        continue;
                    }
                    let has_key = marker.source_row.is_some()
                        || (marker.stable_id.is_some() && dataset_id.is_some());
                    let best = if has_key {
                        &mut best_keyed
                    } else {
                        &mut best_unkeyed
                    };
                    match best {
                        Some((_, best_distance)) if distance >= *best_distance => {}
                        _ => {
                            let item_key = marker
                                .source_row
                                .as_ref()
                                .map(|(dataset, row)| {
                                    StableRowKey::new(dataset.clone(), row.clone())
                                })
                                .or_else(|| {
                                    marker.stable_id.as_ref().and_then(|stable_id| {
                                        dataset_id.as_ref().map(|dataset_id| {
                                            StableRowKey::new(dataset_id.clone(), stable_id.clone())
                                        })
                                    })
                                });
                            *best = Some((
                                OverlayPickCandidate {
                                    sensor_position,
                                    item_key,
                                },
                                distance,
                            ));
                        }
                    }
                }
            }
        }
    }

    best_keyed.or(best_unkeyed).map(|(candidate, _)| candidate)
}

fn overlay_candidate_row(
    candidate: &OverlayPickCandidate,
    investigation_points: &[Investigation2dPoint],
) -> Option<StableRowKey> {
    candidate.item_key.clone().or_else(|| {
        pick_investigation_point(
            investigation_points,
            (
                candidate.sensor_position.x.round() as u16,
                candidate.sensor_position.y.round() as u16,
            ),
        )
        .map(|point| point.item_key.clone())
    })
}

fn pick_investigation_point(
    points: &[Investigation2dPoint],
    sensor: (u16, u16),
) -> Option<&Investigation2dPoint> {
    let sensor = egui::pos2(f32::from(sensor.0), f32::from(sensor.1));
    let mut best: Option<(&Investigation2dPoint, f32)> = None;
    for point in points {
        let point_pos = egui::pos2(point.position[0] as f32, point.position[1] as f32);
        let distance = point_pos.distance(sensor);
        if distance > 8.0 {
            continue;
        }
        match best {
            Some((_, best_distance)) if distance >= best_distance => {}
            _ => best = Some((point, distance)),
        }
    }
    best.map(|(point, _)| point)
}

fn paint_investigation_points(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    points: &[Investigation2dPoint],
    selected_row: Option<&StableRowKey>,
    hovered_row: Option<&StableRowKey>,
) {
    for point in points {
        let screen = viewport.sensor_to_screen(
            image_rect,
            egui::pos2(point.position[0] as f32, point.position[1] as f32),
        );
        let is_selected = selected_row == Some(&point.item_key);
        let is_hovered = hovered_row == Some(&point.item_key);
        let radius = (point.size.max(2.0)
            + if is_selected {
                2.0
            } else if is_hovered {
                1.2
            } else {
                0.0
            })
        .max(2.0);
        let color = egui::Color32::from_rgba_unmultiplied(
            point.color[0],
            point.color[1],
            point.color[2],
            point.color[3],
        );
        paint_marker_shape(
            painter,
            screen,
            marker_shape_from_host(point.marker_shape),
            radius,
            color,
            Some(egui::Stroke::new(
                if is_selected { 2.0 } else { 1.0 },
                if is_selected {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_black_alpha(160)
                },
            )),
        );
    }
}

fn marker_shape_from_host(shape: augur_plugin_api::HostMarkerShape) -> MarkerShape {
    match shape {
        augur_plugin_api::HostMarkerShape::Circle
        | augur_plugin_api::HostMarkerShape::FilledCircle => MarkerShape::FilledCircle,
        augur_plugin_api::HostMarkerShape::Square | augur_plugin_api::HostMarkerShape::Box => {
            MarkerShape::Box
        }
        augur_plugin_api::HostMarkerShape::Diamond => MarkerShape::Diamond,
        augur_plugin_api::HostMarkerShape::Cross => MarkerShape::Cross,
        augur_plugin_api::HostMarkerShape::Point => MarkerShape::Point,
        augur_plugin_api::HostMarkerShape::Ellipse => MarkerShape::Ellipse,
    }
}

fn paint_marker_shape(
    painter: &egui::Painter,
    center: egui::Pos2,
    shape: MarkerShape,
    size: f32,
    fill: egui::Color32,
    stroke: Option<egui::Stroke>,
) {
    let stroke =
        stroke.unwrap_or_else(|| egui::Stroke::new(1.0, egui::Color32::from_black_alpha(160)));
    match shape {
        MarkerShape::Point => {
            painter.circle_filled(center, size.max(1.5), fill);
        }
        MarkerShape::Cross => {
            let arm = size.max(3.0);
            painter.line_segment(
                [
                    egui::pos2(center.x - arm, center.y),
                    egui::pos2(center.x + arm, center.y),
                ],
                egui::Stroke::new(stroke.width + 1.0, egui::Color32::from_black_alpha(120)),
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, center.y - arm),
                    egui::pos2(center.x, center.y + arm),
                ],
                egui::Stroke::new(stroke.width + 1.0, egui::Color32::from_black_alpha(120)),
            );
            painter.line_segment(
                [
                    egui::pos2(center.x - arm, center.y),
                    egui::pos2(center.x + arm, center.y),
                ],
                egui::Stroke::new(stroke.width.max(1.5), fill),
            );
            painter.line_segment(
                [
                    egui::pos2(center.x, center.y - arm),
                    egui::pos2(center.x, center.y + arm),
                ],
                egui::Stroke::new(stroke.width.max(1.5), fill),
            );
        }
        MarkerShape::Box => {
            let rect = egui::Rect::from_center_size(center, egui::vec2(size * 2.0, size * 2.0));
            painter.rect_filled(rect, 1.0, fill);
            painter.rect_stroke(rect, 1.0, stroke);
        }
        MarkerShape::Ellipse => {
            let radii = egui::vec2(size * 1.2, size);
            let points: [egui::Pos2; 25] = std::array::from_fn(|step| {
                let angle = std::f32::consts::TAU * step as f32 / 24.0;
                egui::pos2(
                    center.x + radii.x * angle.cos(),
                    center.y + radii.y * angle.sin(),
                )
            });
            painter.add(egui::Shape::convex_polygon(Vec::from(points), fill, stroke));
        }
        MarkerShape::Diamond => {
            let points = vec![
                egui::pos2(center.x, center.y - size),
                egui::pos2(center.x + size, center.y),
                egui::pos2(center.x, center.y + size),
                egui::pos2(center.x - size, center.y),
            ];
            painter.add(egui::Shape::convex_polygon(points, fill, stroke));
        }
        MarkerShape::FilledCircle => {
            painter.circle_filled(center, size.max(2.0), fill);
            painter.circle_stroke(center, size.max(2.0), stroke);
        }
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

#[cfg(test)]
fn build_preview_viewport(
    frame: &PreviewFrame,
    config: &CameraConfig,
    annotation_manager: &AnnotationManager,
    workspace: &mut PreviewWorkspaceState,
) -> PreviewViewport {
    build_preview_viewport_with_options(frame, config, annotation_manager, workspace, false)
}

fn build_preview_viewport_with_options(
    frame: &PreviewFrame,
    config: &CameraConfig,
    annotation_manager: &AnnotationManager,
    workspace: &mut PreviewWorkspaceState,
    auto_focus_roi: bool,
) -> PreviewViewport {
    workspace.zoom = workspace.zoom.clamp(PREVIEW_ZOOM_MIN, PREVIEW_ZOOM_MAX);

    let full_sensor_rect = egui::Rect::from_min_size(
        egui::Pos2::ZERO,
        egui::vec2(frame.width as f32, frame.height as f32),
    );
    let roi_rect = roi_sensor_rect(config, frame);
    let base_sensor_rect = crop_target_sensor_rect(config, frame, annotation_manager, workspace)
        .or_else(|| {
            auto_focus_roi.then_some(roi_rect).flatten().filter(|roi| {
                roi.width() < full_sensor_rect.width() * 0.95
                    || roi.height() < full_sensor_rect.height() * 0.95
            })
        })
        .or_else(|| {
            auto_focus_roi
                .then(|| active_frame_sensor_rect(frame))
                .flatten()
                .filter(|rect| {
                    rect.width() < full_sensor_rect.width() * 0.92
                        || rect.height() < full_sensor_rect.height() * 0.92
                })
        })
        .unwrap_or(full_sensor_rect);

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

fn crop_target_sensor_rect(
    config: &CameraConfig,
    frame: &PreviewFrame,
    annotation_manager: &AnnotationManager,
    workspace: &mut PreviewWorkspaceState,
) -> Option<egui::Rect> {
    match workspace.crop_target {
        PreviewCropTarget::None => None,
        PreviewCropTarget::HardwareRoi => roi_sensor_rect(config, frame),
        PreviewCropTarget::Annotation(id) => {
            let Some(annotation) = annotation_manager.annotation(id) else {
                workspace.crop_target = PreviewCropTarget::None;
                return None;
            };
            sensor_rect_from_annotation_shape(&annotation.shape, frame)
        }
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

fn active_frame_sensor_rect(frame: &PreviewFrame) -> Option<egui::Rect> {
    const MIN_ACTIVE_PIXELS_FOR_AUTOFIT: usize = 64;
    const ACTIVE_BOUNDS_PADDING_FRACTION: f32 = 0.08;
    const ACTIVE_BOUNDS_MIN_PADDING: f32 = 12.0;

    let width = usize::from(frame.width);
    let height = usize::from(frame.height);
    let pixel_count = width.checked_mul(height)?;
    if width == 0 || height == 0 || frame.pixels.len() < pixel_count {
        return None;
    }

    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut active = 0usize;

    for (idx, &value) in frame.pixels.iter().take(pixel_count).enumerate() {
        if value == 0 {
            continue;
        }
        let x = idx % width;
        let y = idx / width;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        active += 1;
    }

    if active < MIN_ACTIVE_PIXELS_FOR_AUTOFIT {
        return None;
    }

    let raw_width = (max_x.saturating_sub(min_x) + 1) as f32;
    let raw_height = (max_y.saturating_sub(min_y) + 1) as f32;
    let pad_x = (raw_width * ACTIVE_BOUNDS_PADDING_FRACTION).max(ACTIVE_BOUNDS_MIN_PADDING);
    let pad_y = (raw_height * ACTIVE_BOUNDS_PADDING_FRACTION).max(ACTIVE_BOUNDS_MIN_PADDING);

    Some(egui::Rect::from_min_max(
        egui::pos2(
            (min_x as f32 - pad_x).max(0.0),
            (min_y as f32 - pad_y).max(0.0),
        ),
        egui::pos2(
            ((max_x + 1) as f32 + pad_x).min(frame.width as f32),
            ((max_y + 1) as f32 + pad_y).min(frame.height as f32),
        ),
    ))
}

fn sensor_rect_from_annotation_shape(
    shape: &AnnotationShape,
    frame: &PreviewFrame,
) -> Option<egui::Rect> {
    if frame.width == 0 || frame.height == 0 {
        return None;
    }

    let bounds = shape.bounds_rect();
    let min_x = f32::from(bounds.min.0.min(frame.width.saturating_sub(1)));
    let min_y = f32::from(bounds.min.1.min(frame.height.saturating_sub(1)));
    let max_x = f32::from(
        bounds
            .max
            .0
            .min(frame.width.saturating_sub(1))
            .saturating_add(1),
    );
    let max_y = f32::from(
        bounds
            .max
            .1
            .min(frame.height.saturating_sub(1))
            .saturating_add(1),
    );
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

fn paint_analysis_overlays(
    painter: &egui::Painter,
    image_rect: egui::Rect,
    viewport: PreviewViewport,
    overlays: &[Overlay],
) {
    for overlay in overlays {
        match overlay {
            Overlay::HighlightPixels { pixels, color } => {
                let fill =
                    egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                for pixel in pixels {
                    let min = viewport.sensor_to_screen(
                        image_rect,
                        egui::pos2(f32::from(pixel.x), f32::from(pixel.y)),
                    );
                    let max = viewport.sensor_to_screen(
                        image_rect,
                        egui::pos2(
                            f32::from(pixel.x.saturating_add(1)),
                            f32::from(pixel.y.saturating_add(1)),
                        ),
                    );
                    painter.rect_filled(egui::Rect::from_min_max(min, max), 0.0, fill);
                }
            }
            Overlay::CrosshairMarkers {
                markers,
                color,
                arm_len,
            } => {
                let overlay_color =
                    egui::Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]);
                let shadow = egui::Color32::from_rgba_premultiplied(0, 0, 0, color[3].min(180));
                for marker in markers {
                    let center =
                        viewport.sensor_to_screen(image_rect, egui::pos2(marker.x, marker.y));
                    let arm_x = image_rect.width()
                        * (f32::from(*arm_len) / viewport.visible_sensor_rect.width().max(1.0));
                    let arm_y = image_rect.height()
                        * (f32::from(*arm_len) / viewport.visible_sensor_rect.height().max(1.0));
                    let horizontal = [
                        egui::pos2(center.x - arm_x, center.y),
                        egui::pos2(center.x + arm_x, center.y),
                    ];
                    let vertical = [
                        egui::pos2(center.x, center.y - arm_y),
                        egui::pos2(center.x, center.y + arm_y),
                    ];
                    painter.line_segment(horizontal, egui::Stroke::new(3.0, shadow));
                    painter.line_segment(vertical, egui::Stroke::new(3.0, shadow));
                    painter.line_segment(horizontal, egui::Stroke::new(1.5, overlay_color));
                    painter.line_segment(vertical, egui::Stroke::new(1.5, overlay_color));
                }
            }
            Overlay::MarkerOverlay { markers, .. } => {
                for marker in markers {
                    let overlay_color = egui::Color32::from_rgba_unmultiplied(
                        marker.color[0],
                        marker.color[1],
                        marker.color[2],
                        marker.color[3],
                    );
                    let screen =
                        viewport.sensor_to_screen(image_rect, egui::pos2(marker.x, marker.y));
                    paint_marker_shape(
                        painter,
                        screen,
                        marker.shape,
                        marker.size.max(2.0),
                        overlay_color,
                        Some(egui::Stroke::new(1.0, egui::Color32::from_black_alpha(140))),
                    );
                }
            }
        }
    }
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

fn preview_heading(mode: AppMode) -> &'static str {
    match mode {
        AppMode::Replaying => "Replay",
        _ => "Live Preview",
    }
}

fn empty_preview_message(mode: AppMode, camera_detected: bool) -> &'static str {
    match mode {
        AppMode::Idle if camera_detected => {
            "Camera detected. Start Preview or Record, or open a replay file."
        }
        AppMode::Idle => "No camera detected. Probe the camera, or open a replay file.",
        AppMode::Replaying => {
            "No replay frame yet. Use the timeline or wait for playback to decode the next frame."
        }
        _ => "No preview yet. Probe the camera, then click Preview or Record.",
    }
}

fn analysis_warning_color(severity: AnalysisSeverity, visuals: &egui::Visuals) -> egui::Color32 {
    match severity {
        AnalysisSeverity::Info => egui::Color32::from_rgb(30, 100, 220),
        AnalysisSeverity::Warning => visuals.warn_fg_color,
        AnalysisSeverity::Error => visuals.error_fg_color,
    }
}

pub(crate) fn draw_replay_transport(
    ui: &mut egui::Ui,
    state: &mut ViewerState,
    replay: ViewerReplayState,
    viewer_id: &str,
    output: &mut ViewerOutput,
) {
    if !replay.active {
        return;
    }

    let has_duration = replay.duration_us > 0;
    let time_text = if has_duration {
        format!(
            "{} / {}",
            format_replay_time(replay.time_us),
            format_replay_time(replay.duration_us)
        )
    } else {
        String::new()
    };

    let mut new_speed: Option<f32> = None;

    ui.horizontal(|ui| {
        let transport_button_size =
            egui::vec2(ui.spacing().interact_size.y, ui.spacing().interact_size.y);
        let play_pause = if replay.paused {
            phosphor::PLAY
        } else {
            phosphor::PAUSE
        };
        if ui
            .add(
                egui::Button::new(toolbar_icon(phosphor::ARROW_CLOCKWISE))
                    .min_size(transport_button_size),
            )
            .on_hover_text("Restart replay")
            .clicked()
        {
            output.replay_restart = true;
        }

        if ui
            .add_enabled(
                replay.paused,
                egui::Button::new(toolbar_icon(phosphor::CARET_LEFT))
                    .min_size(transport_button_size),
            )
            .on_hover_text("Step back one frame (\u{2190})")
            .clicked()
        {
            output.replay_step_frames = Some(-1);
        }

        if ui
            .add_enabled(
                !replay.finished,
                egui::Button::new(toolbar_icon(play_pause)).min_size(transport_button_size),
            )
            .on_hover_text(if replay.paused {
                "Play replay (Space)"
            } else {
                "Pause replay (Space)"
            })
            .clicked()
        {
            output.replay_toggle_pause = true;
        }

        if ui
            .add_enabled(
                !replay.finished,
                egui::Button::new(toolbar_icon(phosphor::CARET_RIGHT))
                    .min_size(transport_button_size),
            )
            .on_hover_text("Step forward one frame (\u{2192})")
            .clicked()
        {
            output.replay_step_frames = Some(1);
        }

        if ui
            .add(egui::Button::new(toolbar_icon(phosphor::STOP)).min_size(transport_button_size))
            .on_hover_text("Close replay")
            .clicked()
        {
            output.replay_stop = true;
        }

        crate::theme::toolbar_separator(ui);

        let selected_label = replay_speed_label(replay.speed);
        egui::ComboBox::from_id_source((viewer_id, "replay_speed_combo"))
            .selected_text(selected_label)
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

        // Pre-compute so we can reserve its width in the scrubber calculation below.
        let mb_text = format!(
            "{:.1} / {:.1} MB",
            replay.bytes_read as f64 / (1024.0 * 1024.0),
            replay.data_len as f64 / (1024.0 * 1024.0)
        );

        if has_duration {
            crate::theme::toolbar_separator(ui);

            let time_width =
                ui.fonts(|f| {
                    f.layout_no_wrap(
                        time_text.clone(),
                        egui::FontId::default(),
                        ui.visuals().text_color(),
                    )
                })
                .size()
                .x + ui.spacing().item_spacing.x * 2.0;

            let mb_width = {
                let small_font = egui::TextStyle::Small.resolve(ui.style());
                ui.fonts(|f| f.layout_no_wrap(mb_text.clone(), small_font, egui::Color32::WHITE))
                    .size()
                    .x
            } + ui.spacing().item_spacing.x * 3.0
                + 1.0; // separator gap + padding

            let mut timeline_fraction = state.replay_seek_drag.unwrap_or(replay.fraction);
            let slider_width = (ui.available_width() - time_width - mb_width).max(80.0);
            let track_height: f32 = 14.0;
            let (track_rect, response) = ui.allocate_exact_size(
                egui::vec2(slider_width, track_height),
                egui::Sense::click_and_drag(),
            );
            if let Some(pos) = response.interact_pointer_pos() {
                if response.dragged() || response.clicked() {
                    let f = ((pos.x - track_rect.left()) / track_rect.width()).clamp(0.0, 1.0);
                    timeline_fraction = f;
                    state.replay_seek_drag = Some(f);
                }
            }
            if response.drag_stopped() || response.clicked() {
                state.replay_seek_drag = None;
                output.replay_seek_to = Some(timeline_fraction);
            }
            // Custom track: 4 px ink-filled bar with circular thumb on top.
            let palette = crate::theme::palette_for_visuals(ui.visuals());
            let bar_height = 4.0;
            let bar_y = track_rect.center().y - bar_height * 0.5;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(track_rect.left(), bar_y),
                egui::vec2(track_rect.width(), bar_height),
            );
            ui.painter().rect_filled(bar_rect, 2.0, palette.bg_3);
            let fill_rect = egui::Rect::from_min_size(
                bar_rect.min,
                egui::vec2(bar_rect.width() * timeline_fraction, bar_height),
            );
            ui.painter().rect_filled(fill_rect, 2.0, palette.ink);
            let thumb_x = bar_rect.left() + bar_rect.width() * timeline_fraction;
            let thumb_radius = 6.0;
            ui.painter().circle_filled(
                egui::pos2(thumb_x, track_rect.center().y),
                thumb_radius,
                palette.bg_1,
            );
            ui.painter().circle_stroke(
                egui::pos2(thumb_x, track_rect.center().y),
                thumb_radius,
                egui::Stroke::new(2.0, palette.ink),
            );

            ui.label(&time_text);
        }

        ui.separator();
        ui.small(&mb_text);
    });

    if let Some(speed) = new_speed {
        output.replay_set_speed = Some(speed);
    }
}

fn handle_replay_shortcuts(
    ctx: &egui::Context,
    replay: ViewerReplayState,
    output: &mut ViewerOutput,
) {
    if !replay.active || ctx.wants_keyboard_input() {
        return;
    }

    let (frame_steps, toggle_pause) = ctx.input_mut(|input| {
        let forward_steps =
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) as i64;
        let backward_steps =
            input.count_and_consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) as i64;
        let toggle_pause = input.consume_key(egui::Modifiers::NONE, egui::Key::Space);
        (forward_steps - backward_steps, toggle_pause)
    });

    if frame_steps != 0 {
        if !replay.paused {
            output.replay_toggle_pause = true;
        }
        output.replay_step_frames = Some(frame_steps);
        return;
    }

    if toggle_pause && !replay.finished {
        output.replay_toggle_pause = true;
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

pub(crate) fn replay_speed_matches(current: f32, candidate: f32) -> bool {
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
        build_preview_viewport, build_preview_viewport_with_options, pick_overlay_candidate,
        sensor_rect_to_roi_config, PreviewCropTarget, PreviewWorkspaceState, PREVIEW_ZOOM_MAX,
    };
    use crate::investigation::StableRowKey;
    use crate::viewer_tools::{AnnotationManager, AnnotationShapeKind};
    use augur_core::{
        analysis::{MarkerOverlayItem, MarkerShape, Overlay, Pixel},
        config::CameraConfig,
        pipeline::PreviewFrame,
    };

    fn test_frame() -> PreviewFrame {
        PreviewFrame {
            width: 1280,
            height: 720,
            pixels: vec![0; 1280 * 720],
            pixels_on: vec![0; 1280 * 720],
            pixels_off: vec![0; 1280 * 720],
            cached_total_histogram: Vec::new(),
            cached_signed_histogram: Vec::new(),
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            external_triggers: Vec::new(),
            window_start_us: 0,
            window_end_us: 0,
        }
    }

    fn annotation_manager_with_rectangle() -> AnnotationManager {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Rectangle, (100, 120));
        annotations.update_drawing((220, 260));
        annotations.finish_drawing();
        annotations
    }

    fn annotation_manager_with_ellipse() -> AnnotationManager {
        let mut annotations = AnnotationManager::default();
        annotations.start_drawing(AnnotationShapeKind::Ellipse, (200, 150));
        annotations.update_drawing((320, 270));
        annotations.finish_drawing();
        annotations
    }

    #[test]
    fn preview_viewport_uses_hardware_roi_when_crop_is_enabled() {
        let frame = test_frame();
        let mut config = CameraConfig::default();
        config.roi.x = 120;
        config.roi.y = 90;
        config.roi.width = 320;
        config.roi.height = 180;
        let annotations = AnnotationManager::default();

        let mut workspace = PreviewWorkspaceState {
            crop_target: PreviewCropTarget::HardwareRoi,
            ..Default::default()
        };
        let viewport = build_preview_viewport(&frame, &config, &annotations, &mut workspace);

        assert_eq!(viewport.base_sensor_rect.min, egui::pos2(120.0, 90.0));
        assert_eq!(viewport.base_sensor_rect.size(), egui::vec2(320.0, 180.0));
    }

    #[test]
    fn preview_viewport_auto_focuses_active_pixels_when_replay_roi_is_missing() {
        let mut frame = test_frame();
        frame.width = 200;
        frame.height = 120;
        frame.pixels = vec![0; 200 * 120];
        frame.pixels_on = vec![0; 200 * 120];
        frame.pixels_off = vec![0; 200 * 120];
        for y in 40..80 {
            for x in 70..130 {
                frame.pixels[y * 200 + x] = 1;
            }
        }
        let config = CameraConfig::default();
        let annotations = AnnotationManager::default();
        let mut workspace = PreviewWorkspaceState::default();

        let viewport = build_preview_viewport_with_options(
            &frame,
            &config,
            &annotations,
            &mut workspace,
            true,
        );

        assert!(viewport.base_sensor_rect.min.x > 0.0);
        assert!(viewport.base_sensor_rect.min.y > 0.0);
        assert!(viewport.base_sensor_rect.max.x < frame.width as f32);
        assert!(viewport.base_sensor_rect.max.y < frame.height as f32);
        assert!(viewport.base_sensor_rect.width() > 60.0);
        assert!(viewport.base_sensor_rect.height() > 40.0);
    }

    #[test]
    fn preview_viewport_clamps_pan_inside_base_rect() {
        let frame = test_frame();
        let config = CameraConfig::default();
        let annotations = AnnotationManager::default();
        let mut workspace = PreviewWorkspaceState {
            zoom: PREVIEW_ZOOM_MAX,
            pan: egui::vec2(10_000.0, -10_000.0),
            ..Default::default()
        };

        let viewport = build_preview_viewport(&frame, &config, &annotations, &mut workspace);

        assert!(viewport.visible_sensor_rect.min.x >= 0.0);
        assert!(viewport.visible_sensor_rect.min.y >= 0.0);
        assert!(viewport.visible_sensor_rect.max.x <= frame.width as f32);
        assert!(viewport.visible_sensor_rect.max.y <= frame.height as f32);
    }

    #[test]
    fn preview_viewport_uses_selected_rectangle_annotation_crop_target() {
        let frame = test_frame();
        let config = CameraConfig::default();
        let annotations = annotation_manager_with_rectangle();
        let mut workspace = PreviewWorkspaceState {
            crop_target: PreviewCropTarget::Annotation(0),
            ..Default::default()
        };

        let viewport = build_preview_viewport(&frame, &config, &annotations, &mut workspace);

        assert_eq!(viewport.base_sensor_rect.min, egui::pos2(100.0, 120.0));
        assert_eq!(viewport.base_sensor_rect.max, egui::pos2(221.0, 261.0));
    }

    #[test]
    fn preview_viewport_uses_ellipse_bounding_box_for_crop_target() {
        let frame = test_frame();
        let config = CameraConfig::default();
        let annotations = annotation_manager_with_ellipse();
        let mut workspace = PreviewWorkspaceState {
            crop_target: PreviewCropTarget::Annotation(0),
            ..Default::default()
        };

        let viewport = build_preview_viewport(&frame, &config, &annotations, &mut workspace);

        assert_eq!(viewport.base_sensor_rect.min, egui::pos2(199.0, 149.0));
        assert_eq!(viewport.base_sensor_rect.max, egui::pos2(322.0, 272.0));
    }

    #[test]
    fn missing_annotation_crop_target_resets_to_full_frame() {
        let frame = test_frame();
        let config = CameraConfig::default();
        let annotations = AnnotationManager::default();
        let mut workspace = PreviewWorkspaceState {
            crop_target: PreviewCropTarget::Annotation(99),
            ..Default::default()
        };

        let viewport = build_preview_viewport(&frame, &config, &annotations, &mut workspace);

        assert_eq!(workspace.crop_target, PreviewCropTarget::None);
        assert_eq!(viewport.base_sensor_rect.min, egui::Pos2::ZERO);
        assert_eq!(
            viewport.base_sensor_rect.size(),
            egui::vec2(frame.width as f32, frame.height as f32)
        );
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

    #[test]
    fn overlay_picker_prefers_keyed_marker_over_closer_unkeyed_highlight() {
        let overlays = vec![
            Overlay::HighlightPixels {
                pixels: vec![Pixel::new(100, 100)],
                color: [255, 0, 0, 255],
            },
            Overlay::MarkerOverlay {
                markers: vec![MarkerOverlayItem {
                    x: 102.0,
                    y: 100.0,
                    shape: MarkerShape::FilledCircle,
                    size: 5.0,
                    color: [0, 255, 0, 255],
                    timestamp_us: None,
                    stable_id: Some("row-7".into()),
                    source_row: None,
                }],
                dataset_id: Some("dataset-a".into()),
                layer_id: None,
                source_label: None,
            },
        ];

        let candidate =
            pick_overlay_candidate(&overlays, (100, 100)).expect("candidate should exist");

        assert_eq!(
            candidate.item_key,
            Some(StableRowKey::new("dataset-a", "row-7"))
        );
        assert_eq!(candidate.sensor_position, egui::pos2(102.0, 100.0));
    }

    #[test]
    fn overlay_picker_prefers_explicit_source_row_over_layer_dataset_id() {
        let overlays = vec![Overlay::MarkerOverlay {
            markers: vec![MarkerOverlayItem {
                x: 42.0,
                y: 42.0,
                shape: MarkerShape::Diamond,
                size: 5.0,
                color: [255, 90, 90, 200],
                timestamp_us: None,
                stable_id: Some("marker-3".into()),
                source_row: Some(("augur.evesmlm.rejected_fits".into(), "row-3".into())),
            }],
            dataset_id: Some("augur.evesmlm.rejected_fits_layer".into()),
            layer_id: None,
            source_label: None,
        }];

        let candidate =
            pick_overlay_candidate(&overlays, (42, 42)).expect("candidate should exist");

        assert_eq!(
            candidate.item_key,
            Some(StableRowKey::new("augur.evesmlm.rejected_fits", "row-3"))
        );
    }
}
