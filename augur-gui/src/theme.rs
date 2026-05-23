//! AugurRS Design System tokens for egui.
//!
//! Single source of truth for chrome colors, spacing, radii, and the
//! `egui::Visuals` configuration that gives the app its scientific-workbench
//! aesthetic. Light is the first-class theme; dark is a faithful mirror.
//!
//! Tokens mirror `colors_and_type.css` from the design bundle. Data colors
//! (LUTs, raw event ON/OFF, polarity-mode ON/OFF) live next to chrome tokens
//! so callers have one place to look. Tokens are theme-aware where the design
//! system provides a dark counterpart, theme-stable where they encode data.

use egui::epaint::Shadow;
use egui::{Color32, Rounding, Stroke, Vec2};

/// 4-based spacing scale (px). Matches `--aug-sp-*`.
#[allow(dead_code)]
pub mod sp {
    pub const SP_0: f32 = 2.0;
    pub const SP_1: f32 = 4.0;
    pub const SP_2: f32 = 8.0;
    pub const SP_3: f32 = 12.0;
    pub const SP_4: f32 = 16.0;
    pub const SP_5: f32 = 20.0;
    pub const SP_6: f32 = 24.0;
    pub const SP_7: f32 = 32.0;
    pub const SP_8: f32 = 40.0;
}

/// Compact corner radii. Matches `--aug-r-*`.
#[allow(dead_code)]
pub mod radius {
    pub const R_1: f32 = 2.0; // chips, inputs
    pub const R_2: f32 = 4.0; // buttons
    pub const R_3: f32 = 6.0; // panels (PANEL_ROUNDING)
    pub const R_4: f32 = 8.0; // large cards
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    pub fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }
}

/// Resolved chrome palette for one theme mode.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    // Surfaces
    pub bg_0: Color32,
    pub bg_1: Color32,
    pub bg_2: Color32,
    pub bg_3: Color32,
    pub bg_4: Color32,
    // Lines
    pub line: Color32,
    pub line_strong: Color32,
    pub line_subtle: Color32,
    // Text
    pub fg_0: Color32,
    pub fg_1: Color32,
    pub fg_2: Color32,
    pub fg_3: Color32,
    pub fg_4: Color32,
    // Brand ink
    pub ink: Color32,
    // Accent
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_press: Color32,
    pub accent_weak: Color32,
    pub focus_ring: Color32,
    // Status
    pub status_success: Color32,
    pub status_info: Color32,
    pub status_warn: Color32,
    pub status_error: Color32,
    pub status_record: Color32,
}

impl Palette {
    pub const fn light() -> Self {
        Self {
            bg_0: rgb(0xfa, 0xfa, 0xfa),
            bg_1: rgb(0xff, 0xff, 0xff),
            bg_2: rgb(0xf2, 0xf3, 0xf5),
            bg_3: rgb(0xe8, 0xea, 0xed),
            bg_4: rgb(0xdc, 0xdf, 0xe3),

            line: rgb(0xd8, 0xdb, 0xe0),
            line_strong: rgb(0xb8, 0xbc, 0xc2),
            line_subtle: rgb(0xec, 0xee, 0xf1),

            fg_0: rgb(0x0f, 0x11, 0x14),
            fg_1: rgb(0x2a, 0x2e, 0x35),
            fg_2: rgb(0x5b, 0x61, 0x6b),
            fg_3: rgb(0x8a, 0x90, 0x9a),
            fg_4: rgb(0xb4, 0xb9, 0xc1),

            ink: rgb(0x11, 0x13, 0x18),

            accent: rgb(0x1e, 0x64, 0xdc),
            accent_hover: rgb(0x15, 0x53, 0xbe),
            accent_press: rgb(0x0e, 0x42, 0x99),
            accent_weak: rgb(0xe6, 0xee, 0xfb),
            focus_ring: rgb(0x4b, 0x8d, 0xf8),

            status_success: rgb(0x00, 0xa0, 0x3c),
            status_info: rgb(0x1e, 0x64, 0xdc),
            status_warn: rgb(0xc4, 0x7a, 0x00),
            status_error: rgb(0xd0, 0x32, 0x32),
            status_record: rgb(0xdc, 0x32, 0x32),
        }
    }

    pub const fn dark() -> Self {
        Self {
            bg_0: rgb(0x10, 0x12, 0x17),
            bg_1: rgb(0x15, 0x18, 0x1e),
            bg_2: rgb(0x1b, 0x1f, 0x26),
            bg_3: rgb(0x23, 0x28, 0x30),
            bg_4: rgb(0x2c, 0x32, 0x3c),

            line: rgb(0x2a, 0x2f, 0x38),
            line_strong: rgb(0x3b, 0x42, 0x4d),
            line_subtle: rgb(0x1e, 0x23, 0x2b),

            fg_0: rgb(0xf1, 0xf3, 0xf6),
            fg_1: rgb(0xd8, 0xdc, 0xe2),
            fg_2: rgb(0xa2, 0xa9, 0xb4),
            fg_3: rgb(0x75, 0x7d, 0x88),
            fg_4: rgb(0x54, 0x5b, 0x64),

            ink: rgb(0xf1, 0xf3, 0xf6),

            accent: rgb(0x6e, 0xa8, 0xff),
            accent_hover: rgb(0x8d, 0xba, 0xff),
            accent_press: rgb(0x4b, 0x8d, 0xf8),
            accent_weak: rgb(0x1a, 0x27, 0x40),
            focus_ring: rgb(0x6e, 0xa8, 0xff),

            status_success: rgb(0x4f, 0xd6, 0x8a),
            status_info: rgb(0x6e, 0xa8, 0xff),
            status_warn: rgb(0xff, 0xc1, 0x5c),
            status_error: rgb(0xff, 0x6e, 0x6e),
            status_record: rgb(0xdc, 0x32, 0x32),
        }
    }

    pub const fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// Raw event ON colour (warm amber). RGBA32 with the same alpha the painter
/// uses for marker overlays. Theme-stable — events encode data.
pub const RAW_EVENT_ON_RGBA: [u8; 4] = [0xff, 0xba, 0x5c, 240];
/// Raw event OFF colour (cool teal). Theme-stable.
pub const RAW_EVENT_OFF_RGBA: [u8; 4] = [0x5c, 0xd6, 0xc9, 220];

/// Polarity-mode ON (hot magenta). Replaces the generic red half of the
/// red/blue cliché when rendering the polarity preview.
pub const POLARITY_ON_RGB: [u8; 3] = [0xe8, 0x3e, 0xb0];
/// Polarity-mode OFF (arctic cyan). Replaces the generic blue half.
pub const POLARITY_OFF_RGB: [u8; 3] = [0x00, 0xb8, 0xd4];

/// ROI / annotation accent colours from the design bundle.
pub const ROI_AMBER: Color32 = rgb(0xff, 0xc4, 0x40);
pub const ROI_CYAN: Color32 = rgb(0x40, 0xdc, 0xff);

/// Pick the right palette for the active visuals.
pub fn palette_for_visuals(visuals: &egui::Visuals) -> Palette {
    if visuals.dark_mode {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// Build the AugurRS `egui::Visuals` for a given mode.
///
/// This replaces `egui::Visuals::dark()` / `light()`. It tunes panel/window
/// fills, hairline strokes, hover/press tints, focus ring, hyperlink and
/// status text so the chrome reads as a dense scientific instrument panel.
pub fn visuals(mode: ThemeMode) -> egui::Visuals {
    let p = Palette::for_mode(mode);
    let mut visuals = if mode.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.dark_mode = mode.is_dark();

    visuals.panel_fill = p.bg_0;
    visuals.window_fill = p.bg_1;
    visuals.window_stroke = Stroke::new(1.0, p.line);
    visuals.window_rounding = Rounding::same(radius::R_3);
    visuals.menu_rounding = Rounding::same(radius::R_3);

    visuals.faint_bg_color = p.bg_2;
    visuals.extreme_bg_color = p.bg_1;
    visuals.code_bg_color = p.bg_2;

    visuals.hyperlink_color = p.accent;
    visuals.warn_fg_color = p.status_warn;
    visuals.error_fg_color = p.status_error;

    visuals.selection.bg_fill = p.accent_weak;
    visuals.selection.stroke = Stroke::new(1.0, p.accent);

    // Subtle, restrained shadows — design says "1px outline preferred over a shadow".
    visuals.window_shadow = Shadow {
        offset: Vec2::new(0.0, 2.0),
        blur: 6.0,
        spread: 0.0,
        color: if mode.is_dark() {
            Color32::from_black_alpha(160)
        } else {
            Color32::from_black_alpha(20)
        },
    };
    visuals.popup_shadow = visuals.window_shadow;

    // Buttons & widgets
    let widgets = &mut visuals.widgets;
    let widget_rounding = Rounding::same(radius::R_2);

    // Noninteractive: panel separators, frame outlines, default text.
    widgets.noninteractive.bg_fill = p.bg_1;
    widgets.noninteractive.weak_bg_fill = p.bg_1;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.fg_1);
    widgets.noninteractive.rounding = widget_rounding;
    widgets.noninteractive.expansion = 0.0;

    // Inactive: at-rest button.
    widgets.inactive.bg_fill = p.bg_2;
    widgets.inactive.weak_bg_fill = p.bg_1;
    widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
    widgets.inactive.fg_stroke = Stroke::new(1.0, p.fg_1);
    widgets.inactive.rounding = widget_rounding;
    widgets.inactive.expansion = 0.0;

    // Hovered: ~6% darker fill (light) / lighter fill (dark), no scaling.
    widgets.hovered.bg_fill = p.bg_3;
    widgets.hovered.weak_bg_fill = p.bg_2;
    widgets.hovered.bg_stroke = Stroke::new(1.0, p.line_strong);
    widgets.hovered.fg_stroke = Stroke::new(1.0, p.fg_0);
    widgets.hovered.rounding = widget_rounding;
    widgets.hovered.expansion = 0.0;

    // Active (pressed): one step darker still.
    widgets.active.bg_fill = p.bg_4;
    widgets.active.weak_bg_fill = p.bg_3;
    widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    widgets.active.fg_stroke = Stroke::new(1.0, p.fg_0);
    widgets.active.rounding = widget_rounding;
    widgets.active.expansion = 0.0;

    // Focus ring uses Visuals::selection.stroke — already wired above. Make
    // the open dropdown / focused-input visual pick up the accent color.
    widgets.open.bg_fill = p.accent_weak;
    widgets.open.weak_bg_fill = p.bg_2;
    widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
    widgets.open.fg_stroke = Stroke::new(1.0, p.fg_0);
    widgets.open.rounding = widget_rounding;
    widgets.open.expansion = 0.0;

    visuals.indent_has_left_vline = false;
    visuals.button_frame = true;
    visuals.collapsing_header_frame = false;
    visuals.striped = true;
    visuals.slider_trailing_fill = true;

    visuals.text_cursor = Stroke::new(1.0, p.accent);

    visuals
}

/// AugurRS card frame: background fill on `bg_1`, single hairline stroke,
/// 6px corners, dense interior padding. The design system says cards are
/// flat — use this instead of `egui::Frame::group` for panels in the
/// settings / analysis / investigation surfaces.
pub fn card_frame(ui: &egui::Ui) -> egui::Frame {
    let p = palette_for_visuals(ui.visuals());
    egui::Frame::none()
        .fill(p.bg_1)
        .stroke(Stroke::new(1.0, p.line))
        .rounding(Rounding::same(radius::R_3))
        .inner_margin(egui::Margin::symmetric(sp::SP_3, sp::SP_3))
}

/// Clamp a sub-area to the available/clip width and enable wrapping, so dense
/// single-row sections don't overflow their panel. Returns the resolved width.
pub fn constrain_section_width(ui: &mut egui::Ui) -> f32 {
    let width = ui.available_width().min(ui.clip_rect().width()).max(0.0);
    ui.set_min_width(width);
    ui.set_max_width(width);
    ui.set_width(width);
    ui.style_mut().wrap = Some(true);
    width
}

/// Render a terse uppercase section subhead — the workbench design uses
/// these to label sub-areas inside dense panels (`SETTINGS`, `LAYERS`,
/// `STATUS & WARNINGS`).
pub fn section_subhead(ui: &mut egui::Ui, text: &str) {
    let p = palette_for_visuals(ui.visuals());
    ui.add_space(sp::SP_1);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(11.0)
            .color(p.fg_2)
            .strong(),
    );
    ui.add_space(sp::SP_0);
}

/// Tone for chips, metric pills and inline status badges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Success,
    Info,
    Warn,
    Error,
}

impl Tone {
    fn fg(self, p: &Palette) -> Color32 {
        match self {
            Self::Neutral => p.fg_2,
            Self::Success => p.status_success,
            Self::Info => p.status_info,
            Self::Warn => p.status_warn,
            Self::Error => p.status_error,
        }
    }

    fn bg(self, p: &Palette) -> Color32 {
        let fg = self.fg(p);
        if matches!(self, Self::Neutral) {
            p.bg_2
        } else {
            // Tint the foreground colour toward the panel background so the
            // chip reads as muted-coloured fill, not a saturated badge.
            let alpha = if p.bg_0.r() > 200 { 32 } else { 56 };
            Color32::from_rgba_unmultiplied(fg.r(), fg.g(), fg.b(), alpha)
        }
    }

    fn border(self, p: &Palette) -> Color32 {
        if matches!(self, Self::Neutral) {
            p.line
        } else {
            Color32::TRANSPARENT
        }
    }
}

/// Render a small monospace pill chip. Use for plugin phase, dirty
/// indicators, and the layer-count badge in the analysis panel.
pub fn chip(ui: &mut egui::Ui, text: &str, tone: Tone) {
    let p = palette_for_visuals(ui.visuals());
    let fg = tone.fg(&p);
    let bg = tone.bg(&p);
    let border = tone.border(&p);

    let font = egui::FontId::monospace(11.0);
    let text_galley = ui.fonts(|fonts| fonts.layout_no_wrap(text.to_owned(), font.clone(), fg));
    let pad_x = 7.0;
    let pad_y = 2.0;
    let size = egui::vec2(
        text_galley.size().x + pad_x * 2.0,
        text_galley.size().y + pad_y * 2.0,
    );
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let radius = rect.height() * 0.5;
    ui.painter().rect_filled(rect, radius, bg);
    if border != Color32::TRANSPARENT {
        ui.painter()
            .rect_stroke(rect, radius, Stroke::new(1.0, border));
    }
    let text_pos = egui::pos2(rect.left() + pad_x, rect.top() + pad_y);
    ui.painter().galley(text_pos, text_galley, fg);
}

/// Style a button as the primary action — ink-coloured background, light
/// text, no shadow. Use sparingly: design says one primary per surface.
pub fn primary_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(text).color(Color32::WHITE)).fill(Palette::light().ink)
}

/// Phosphor glyph used by [`panel_header`] for its trailing collapse
/// toggle. Caller picks the side: the design uses `CARET_LEFT` on the
/// right edge of the left settings panel and `CARET_RIGHT` on the left
/// edge of the right analysis panel.
#[derive(Debug, Clone, Copy)]
pub struct PanelToggle<'a> {
    pub glyph: &'a str,
    pub tooltip: &'a str,
}

/// Render a panel-header row: optional phosphor glyph + uppercase tracked
/// title, optional trailing icon-button that collapses the panel. The row
/// is **flat** (no fill) so it reads as part of the panel body, with a
/// single bottom hairline matching the design's `.panel-header`. Returns
/// `true` when the toggle button was clicked.
///
/// Callers should mount this as the first row inside a `SidePanel`'s
/// body — the bottom hairline doubles as the divider above the panel
/// content, so callers don't need to add their own `ui.separator()`.
pub fn panel_header(
    ui: &mut egui::Ui,
    glyph: Option<&str>,
    text: &str,
    toggle: Option<PanelToggle<'_>>,
) -> bool {
    let p = palette_for_visuals(ui.visuals());
    let mut toggle_clicked = false;
    egui::Frame::none()
        .inner_margin(egui::Margin {
            left: sp::SP_3,
            right: sp::SP_3,
            top: sp::SP_2,
            bottom: sp::SP_2,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = sp::SP_2;
                if let Some(glyph) = glyph {
                    ui.label(egui::RichText::new(glyph).size(13.0).color(p.fg_2));
                }
                ui.label(
                    egui::RichText::new(text.to_uppercase())
                        .size(11.0)
                        .strong()
                        .color(p.ink),
                );
                if let Some(toggle) = toggle {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, toggle.glyph, toggle.tooltip).clicked() {
                            toggle_clicked = true;
                        }
                    });
                }
            });
        });
    ui.separator();
    toggle_clicked
}

/// Compact icon-only button rendered as a single phosphor glyph with a
/// hover background. Used by the panel-header collapse toggle and any
/// other "single-glyph button" the design needs (close ×, expand ▸,
/// kebab …). The framed hover/press states inherit `egui::Visuals` so
/// the button matches the rest of the chrome.
pub fn icon_button(ui: &mut egui::Ui, glyph: &str, tooltip: &str) -> egui::Response {
    let p = palette_for_visuals(ui.visuals());
    let size = egui::vec2(22.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let bg = if response.is_pointer_button_down_on() {
        p.bg_3
    } else if response.hovered() {
        p.bg_2
    } else {
        Color32::TRANSPARENT
    };
    let fg = if response.hovered() { p.fg_0 } else { p.fg_2 };
    let painter = ui.painter();
    if bg != Color32::TRANSPARENT {
        painter.rect_filled(rect, radius::R_2, bg);
    }
    let font = egui::FontId::proportional(13.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(glyph.to_owned(), font.clone(), fg));
    let pos = egui::pos2(
        rect.center().x - galley.size().x * 0.5,
        rect.center().y - galley.size().y * 0.5,
    );
    painter.galley(pos, galley, fg);
    if !tooltip.is_empty() {
        response.clone().on_hover_text(tooltip)
    } else {
        response
    }
}

/// Two-column inspector row used in the right Workspace panel: a left
/// `aug-label` and a right monospace caption value. Mirrors
/// `.inspector-row` from the design.
pub fn inspector_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let p = palette_for_visuals(ui.visuals());
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(label).size(12.0).color(p.fg_2).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(value)
                    .monospace()
                    .size(11.0)
                    .color(p.fg_2),
            );
        });
    });
}

/// Connected pill cluster used by the design's layout toggle. Returns
/// the index of the clicked option, or `None`. The whole strip is
/// painted manually so the active segment inherits the cluster's outer
/// rounding (otherwise an ink-fill on a left/right segment looks like a
/// sharp rectangle inside a rounded frame).
pub fn pill_cluster(
    ui: &mut egui::Ui,
    options: &[&str],
    selected: usize,
    enabled: &[bool],
) -> Option<usize> {
    let p = palette_for_visuals(ui.visuals());
    let radius = radius::R_2;
    let segment_pad_x = sp::SP_2;
    let height = 22.0;

    // Measure each segment label so the layout is single-row, fixed-width.
    let font = egui::FontId::monospace(11.0);
    let widths: Vec<f32> = options
        .iter()
        .map(|label| {
            let g = ui.fonts(|f| f.layout_no_wrap((*label).to_owned(), font.clone(), p.fg_1));
            g.size().x + segment_pad_x * 2.0
        })
        .collect();
    let total_width: f32 = widths.iter().sum();
    let (cluster_rect, _) =
        ui.allocate_exact_size(egui::vec2(total_width, height), egui::Sense::hover());
    // Outer cluster outline.
    ui.painter()
        .rect_stroke(cluster_rect, radius, Stroke::new(1.0, p.line_strong));

    let mut clicked = None;
    let mut x = cluster_rect.left();
    let last_index = options.len().saturating_sub(1);
    for (i, label) in options.iter().enumerate() {
        let w = widths[i];
        let seg_rect =
            egui::Rect::from_min_size(egui::pos2(x, cluster_rect.top()), egui::vec2(w, height));
        x += w;
        let is_on = i == selected;
        let is_enabled = enabled.get(i).copied().unwrap_or(true);
        let fg = if is_on {
            p.bg_1
        } else if is_enabled {
            p.fg_1
        } else {
            p.fg_4
        };
        let painter = ui.painter();
        if is_on {
            // Inset the active fill so it sits inside the outer 1 px border;
            // round only the outer corners that touch the cluster edge.
            let fill_rect = seg_rect.shrink2(egui::vec2(1.0, 1.0));
            let r = if i == 0 && i == last_index {
                Rounding::same(radius - 1.0)
            } else if i == 0 {
                Rounding {
                    nw: radius - 1.0,
                    sw: radius - 1.0,
                    ne: 0.0,
                    se: 0.0,
                }
            } else if i == last_index {
                Rounding {
                    ne: radius - 1.0,
                    se: radius - 1.0,
                    nw: 0.0,
                    sw: 0.0,
                }
            } else {
                Rounding::ZERO
            };
            painter.rect_filled(fill_rect, r, p.ink);
        }
        // Hairline divider between segments.
        if i < last_index {
            painter.line_segment(
                [seg_rect.right_top(), seg_rect.right_bottom()],
                Stroke::new(1.0, p.line_strong),
            );
        }
        let galley = ui.fonts(|f| f.layout_no_wrap((*label).to_owned(), font.clone(), fg));
        let text_pos = egui::pos2(
            seg_rect.center().x - galley.size().x * 0.5,
            seg_rect.center().y - galley.size().y * 0.5,
        );
        painter.galley(text_pos, galley, fg);
        let response = ui
            .interact(
                seg_rect,
                ui.id().with(("pill_cluster_segment", label, i)),
                egui::Sense::click(),
            )
            .on_hover_cursor(if is_enabled {
                egui::CursorIcon::PointingHand
            } else {
                egui::CursorIcon::NotAllowed
            });
        if is_enabled && response.clicked() {
            clicked = Some(i);
        }
    }
    clicked
}

/// AugurRS-styled collapse — chevron + caret + label header with an
/// optional right counter. Body is laid out via the closure. Persists
/// open/closed state by `id_source` like egui's `CollapsingHeader`.
pub fn collapse<R>(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    label: &str,
    default_open: bool,
    right: Option<&str>,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let id = ui.id().with(id_source);
    let mut open = ui
        .ctx()
        .data_mut(|d| d.get_persisted::<bool>(id))
        .unwrap_or(default_open);
    let p = palette_for_visuals(ui.visuals());
    let chevron = if open {
        egui_phosphor::regular::CARET_DOWN
    } else {
        egui_phosphor::regular::CARET_RIGHT
    };
    let header_response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = sp::SP_1;
            let response = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(format!("{chevron}  {label}"))
                            .size(12.0)
                            .strong()
                            .color(p.ink),
                    )
                    .sense(egui::Sense::click()),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            if let Some(right) = right {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(right)
                            .monospace()
                            .size(11.0)
                            .color(p.fg_3),
                    );
                });
            }
            response
        })
        .inner;
    if header_response.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_persisted(id, open));
    }
    if open {
        let r = ui
            .indent(id.with("body"), |ui| {
                ui.add_space(sp::SP_0);
                body(ui)
            })
            .inner;
        Some(r)
    } else {
        None
    }
}

/// Result from [`layer_row`].
pub struct LayerRowResponse {
    pub row: egui::Response,
    pub visible_changed: Option<bool>,
}

/// Compact layer row matching `.layer-row` from the design: eye toggle +
/// 10×10 swatch + name (mono, fg_1) + count (mono, fg_3). Returns the
/// full row response plus the new `visible` value when the eye is clicked.
pub fn layer_row(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    visible: bool,
    color: [u8; 4],
    name: &str,
    count: &str,
) -> LayerRowResponse {
    let p = palette_for_visuals(ui.visuals());
    let row_id = ui.id().with(id_source);
    let mut visible_changed: Option<bool> = None;
    let row_height = 22.0;
    // Allocate the full row width up front so the trailing right-aligned
    // count anchors to the right edge without forcing the parent panel to
    // grow. Without this allocation a nested `right_to_left` block ends up
    // measuring its desired width from content, which expands the right
    // SidePanel when many layers exist.
    let row_response = ui
        .push_id(row_id, |ui| {
            let row_width = ui.available_width();
            ui.allocate_ui_with_layout(
                egui::vec2(row_width, row_height),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.style_mut().wrap = Some(false);
                    ui.spacing_mut().item_spacing.x = sp::SP_2;
                    let glyph = if visible {
                        egui_phosphor::regular::EYE
                    } else {
                        egui_phosphor::regular::EYE_SLASH
                    };
                    ui.push_id("visibility", |ui| {
                        if ui
                            .add(
                                egui::Label::new(
                                    egui::RichText::new(glyph).size(13.0).color(p.fg_2),
                                )
                                .sense(egui::Sense::click()),
                            )
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            visible_changed = Some(!visible);
                        }
                    });
                    // 10×10 swatch with hairline border.
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        rect,
                        radius::R_1,
                        Color32::from_rgba_unmultiplied(color[0], color[1], color[2], color[3]),
                    );
                    ui.painter().rect_stroke(
                        rect,
                        radius::R_1,
                        Stroke::new(1.0, Color32::from_black_alpha(40)),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(name)
                                .monospace()
                                .size(11.0)
                                .color(p.fg_1),
                        )
                        .truncate(true),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(count)
                                .monospace()
                                .size(11.0)
                                .color(p.fg_3),
                        );
                    });
                },
            )
            .response
            .interact(egui::Sense::click())
        })
        .inner;
    LayerRowResponse {
        row: row_response,
        visible_changed,
    }
}

/// Helper for the design's `.field` block — label with optional bracketed
/// `[unit]`. Caller follows up with the input widget(s).
pub fn field_label(ui: &mut egui::Ui, label: &str, unit: Option<&str>) {
    let p = palette_for_visuals(ui.visuals());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = sp::SP_1;
        ui.label(egui::RichText::new(label).size(12.0).color(p.fg_2).strong());
        if let Some(unit) = unit {
            ui.label(
                egui::RichText::new(format!("[{unit}]"))
                    .monospace()
                    .size(11.0)
                    .color(p.fg_3),
            );
        }
    });
}

/// Fixed dark background for the data viewport canvas. Independent of theme.
pub const CANVAS_BG: Color32 = Color32::from_rgb(0x0a, 0x0d, 0x12);

/// 22×22 icon-only toggle button used in the viewer toolbar. Mirrors the
/// design's `.icon-btn` — transparent at rest, `bg-2` on hover,
/// `accent-weak` fill + `accent-press` fg when active.
pub fn icon_toggle_button(
    ui: &mut egui::Ui,
    active: bool,
    glyph: &str,
    tooltip: &str,
) -> egui::Response {
    let p = palette_for_visuals(ui.visuals());
    let size = egui::vec2(22.0, 22.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let bg = if active {
        p.accent_weak
    } else if response.is_pointer_button_down_on() {
        p.bg_3
    } else if response.hovered() {
        p.bg_2
    } else {
        Color32::TRANSPARENT
    };
    let fg = if active {
        p.accent_press
    } else if response.hovered() {
        p.fg_0
    } else {
        p.fg_1
    };

    let painter = ui.painter();
    if bg != Color32::TRANSPARENT {
        painter.rect_filled(rect, radius::R_2, bg);
    }
    let font = egui::FontId::proportional(14.0);
    let galley = ui.fonts(|f| f.layout_no_wrap(glyph.to_owned(), font.clone(), fg));
    let pos = egui::pos2(
        rect.center().x - galley.size().x * 0.5,
        rect.center().y - galley.size().y * 0.5,
    );
    painter.galley(pos, galley, fg);

    let response = if !tooltip.is_empty() {
        response.on_hover_text(tooltip)
    } else {
        response
    };
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Vertical toolbar separator — 1px wide, 18px tall, `line` colour,
/// with 6px horizontal margins on each side.
pub fn toolbar_separator(ui: &mut egui::Ui) {
    let p = palette_for_visuals(ui.visuals());
    let size = egui::vec2(1.0 + 6.0 * 2.0, 18.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let line_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(1.0, 18.0));
    ui.painter().rect_filled(line_rect, 0.0, p.line);
}

/// Apply the standard AugurRS spacing/typography baselines on top of an
/// `egui::Style`. Called once after `set_visuals` so that panel paddings,
/// item spacing and tabular numerals match the design.
pub fn apply_style(style: &mut egui::Style) {
    let s = &mut style.spacing;
    s.item_spacing = Vec2::new(sp::SP_2, sp::SP_1);
    s.button_padding = Vec2::new(sp::SP_2, sp::SP_1);
    s.menu_margin = egui::Margin::symmetric(sp::SP_1, sp::SP_1);
    s.window_margin = egui::Margin::same(sp::SP_3);
    s.indent = sp::SP_4;
    s.interact_size.y = 22.0;
    s.icon_width = 14.0;
    s.icon_width_inner = 8.0;
    s.icon_spacing = sp::SP_1;
    s.combo_width = 140.0;
}
