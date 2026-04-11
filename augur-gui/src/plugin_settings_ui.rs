use augur_plugin_api::{SettingItem, SettingKind, StatusEntry};
use serde_json::json;

use crate::plugin_loader::DynPlugin;

pub fn render_plugin_settings(
    ui: &mut egui::Ui,
    plugin: &mut DynPlugin,
    show_header: bool,
) -> Result<bool, String> {
    let mut changed = false;
    let mut setting_error = None;

    if show_header {
        ui.heading(plugin.name());
        if !plugin.description().is_empty() {
            ui.weak(plugin.description());
        }
    }

    for section in plugin.settings_schema().sections.clone() {
        egui::CollapsingHeader::new(section.label)
            .default_open(section.default_open)
            .show(ui, |ui| {
                if let Some(description) = &section.description {
                    ui.weak(description);
                }

                for item in &section.items {
                    match render_setting_item(ui, plugin, item) {
                        Ok(item_changed) => {
                            if item_changed {
                                changed = true;
                            }
                        }
                        Err(err) => {
                            setting_error = Some(err);
                            break;
                        }
                    }
                }
            });

        if let Some(err) = setting_error.take() {
            return Err(err);
        }
    }

    let status_entries = plugin.status_entries_cached()?;
    if !status_entries.is_empty() {
        ui.separator();
        for entry in &status_entries {
            render_status_entry(ui, entry);
        }
    }

    Ok(changed)
}

fn render_setting_item(
    ui: &mut egui::Ui,
    plugin: &mut DynPlugin,
    item: &SettingItem,
) -> Result<bool, String> {
    match &item.kind {
        SettingKind::Bool { default } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_bool())
                .unwrap_or(*default);
            let response = ui.checkbox(&mut value, &item.label);
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            if response.changed() {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::F64Slider {
            min,
            max,
            default,
            suffix,
        } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_f64())
                .unwrap_or(*default);
            let mut slider = egui::Slider::new(&mut value, *min..=*max).text(&item.label);
            if let Some(suffix) = suffix {
                slider = slider.suffix(suffix);
            }
            let response = ui.add(slider);
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            if response.changed() {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::I64Slider {
            min,
            max,
            default,
            suffix,
        } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_i64())
                .unwrap_or(*default);
            let mut slider = egui::Slider::new(&mut value, *min..=*max).text(&item.label);
            if let Some(suffix) = suffix {
                slider = slider.suffix(suffix);
            }
            let response = ui.add(slider);
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            if response.changed() {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::F64Drag {
            min,
            max,
            speed,
            default,
        } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_f64())
                .unwrap_or(*default);
            let old_value = value;
            let response = ui.horizontal(|ui| {
                ui.label(&item.label);
                ui.add(
                    egui::DragValue::new(&mut value)
                        .clamp_range(*min..=*max)
                        .speed(*speed),
                )
            });
            maybe_add_tooltip(&response.response, item.tooltip.as_deref());
            if value != old_value {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::I64Drag { min, max, default } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_i64())
                .unwrap_or(*default);
            let old_value = value;
            let response = ui.horizontal(|ui| {
                ui.label(&item.label);
                ui.add(egui::DragValue::new(&mut value).clamp_range(*min..=*max))
            });
            maybe_add_tooltip(&response.response, item.tooltip.as_deref());
            if value != old_value {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::Enum { variants, default } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_u64())
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(*default);
            let old_value = value;
            let response = ui.horizontal_wrapped(|ui| {
                ui.label(&item.label);
                for (index, variant) in variants.iter().enumerate() {
                    ui.radio_value(&mut value, index, variant);
                }
            });
            maybe_add_tooltip(&response.response, item.tooltip.as_deref());
            if value != old_value {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
    }

    Ok(false)
}

fn maybe_add_tooltip(response: &egui::Response, tooltip: Option<&str>) {
    if let Some(tooltip) = tooltip {
        response.clone().on_hover_text(tooltip);
    }
}

fn render_status_entry(ui: &mut egui::Ui, entry: &StatusEntry) {
    match entry {
        StatusEntry::Text(text) => {
            ui.label(text);
        }
        StatusEntry::LabeledValue {
            label,
            value,
            color,
        } => {
            if let Some(color) = color {
                ui.colored_label(
                    egui::Color32::from_rgb(color[0], color[1], color[2]),
                    format!("{label}: {value}"),
                );
            } else {
                ui.label(format!("{label}: {value}"));
            }
        }
        StatusEntry::Sparkline {
            label,
            values,
            lower_is_better,
        } => {
            ui.label(label);
            draw_sparkline(ui, values, *lower_is_better);
        }
    }
}

fn draw_sparkline(ui: &mut egui::Ui, values: &[f64], lower_is_better: bool) {
    let desired_size = egui::vec2(ui.available_width().max(180.0), 110.0);
    let (rect, _) = ui.allocate_exact_size(desired_size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(18, 22, 28));
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(72)),
    );

    if values.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Collecting history",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(180),
        );
        return;
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1e-9);
    let width = rect.width().max(1.0);
    let height = rect.height().max(1.0);
    let stroke_color = if lower_is_better {
        egui::Color32::from_rgb(255, 196, 84)
    } else {
        egui::Color32::from_rgb(110, 206, 255)
    };

    let mut points = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        let tx = index as f32 / (values.len().saturating_sub(1)) as f32;
        let ty = ((*value - min) / span) as f32;
        points.push(egui::pos2(
            rect.left() + tx * width,
            rect.bottom() - ty * height,
        ));
    }

    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(2.0, stroke_color),
    ));
}
