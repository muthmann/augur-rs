use std::collections::VecDeque;
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Rect, Rounding, Stroke, Vec2};

/// A single toast notification.
#[derive(Debug, Clone)]
pub struct Toast {
    pub message: String,
    pub tone: ToastTone,
    pub created_at: Instant,
}

/// Tone for the toast left-border accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastTone {
    Info,
    Warn,
    Error,
    Success,
}

impl ToastTone {
    fn border_color(self, visuals: &egui::Visuals) -> Color32 {
        let palette = crate::theme::palette_for_visuals(visuals);
        match self {
            ToastTone::Info => palette.status_info,
            ToastTone::Warn => palette.status_warn,
            ToastTone::Error => palette.status_error,
            ToastTone::Success => palette.status_success,
        }
    }
}

/// Toast notification manager. Displayed as a bottom-right fixed stack.
#[derive(Debug, Default)]
pub struct ToastQueue {
    toasts: VecDeque<Toast>,
}

impl ToastQueue {
    const LIFETIME: Duration = Duration::from_millis(3200);
    const MAX_TOASTS: usize = 5;

    pub fn push(&mut self, message: impl Into<String>, tone: ToastTone) {
        self.toasts.push_back(Toast {
            message: message.into(),
            tone,
            created_at: Instant::now(),
        });
        while self.toasts.len() > Self::MAX_TOASTS {
            self.toasts.pop_front();
        }
    }

    /// Render the toast stack in the bottom-right corner.
    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts
            .retain(|t| now.duration_since(t.created_at) < Self::LIFETIME);

        if self.toasts.is_empty() {
            return;
        }

        let screen = ctx.screen_rect();
        let margin = 16.0;
        let toast_width = 280.0;
        let toast_padding_x = 12.0;
        let toast_padding_y = 7.0;
        let gap = 6.0;
        let border_left = 3.0;

        let mut bottom_right = egui::pos2(screen.right() - margin, screen.bottom() - margin);

        // Render from bottom to top so the newest toast is at the bottom.
        for toast in self.toasts.iter().rev() {
            let palette = crate::theme::palette_for_visuals(&ctx.style().visuals);
            let font = FontId::monospace(12.0);
            let galley = ctx.fonts(|f| {
                f.layout(
                    toast.message.clone(),
                    font,
                    palette.fg_1,
                    toast_width - toast_padding_x * 2.0 - border_left,
                )
            });
            let text_size = galley.size();
            let toast_height = text_size.y + toast_padding_y * 2.0;
            let toast_rect = Rect::from_min_size(
                egui::pos2(bottom_right.x - toast_width, bottom_right.y - toast_height),
                Vec2::new(toast_width, toast_height),
            );

            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("toast_stack"),
            ));

            // Background fill
            painter.rect_filled(
                toast_rect,
                Rounding::same(crate::theme::radius::R_2),
                palette.bg_1,
            );
            // Border
            painter.rect_stroke(
                toast_rect,
                Rounding::same(crate::theme::radius::R_2),
                Stroke::new(1.0, palette.line),
            );
            // Left accent border
            let accent_rect =
                Rect::from_min_size(toast_rect.min, Vec2::new(border_left, toast_rect.height()));
            painter.rect_filled(
                accent_rect,
                Rounding {
                    nw: crate::theme::radius::R_2,
                    sw: crate::theme::radius::R_2,
                    ne: 0.0,
                    se: 0.0,
                },
                toast.tone.border_color(&ctx.style().visuals),
            );
            // Text
            let text_pos = egui::pos2(
                toast_rect.left() + toast_padding_x + border_left,
                toast_rect.top() + toast_padding_y,
            );
            painter.galley(text_pos, galley, palette.fg_1);

            bottom_right.y -= toast_height + gap;
        }
    }
}
