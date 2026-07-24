use augur_plugin_api::{SettingItem, SettingKind, StatusEntry};
use serde_json::json;

use crate::plugin_loader::DynPlugin;

pub fn render_plugin_settings(
    ui: &mut egui::Ui,
    plugin: &mut DynPlugin,
    authoritative_status: Option<&[StatusEntry]>,
    show_header: bool,
    id_source: impl std::hash::Hash,
) -> Result<bool, String> {
    let mut changed = false;

    // Schemas can depend on live plugin state (serial port lists, dynamic
    // slider bounds); refresh at the UI cache cadence instead of trusting
    // the load-time snapshot.
    plugin.refresh_settings_schema_if_stale()?;

    if show_header {
        crate::theme::section_subhead(ui, plugin.name());
        if !plugin.description().is_empty() {
            ui.weak(plugin.description());
        }
    }

    for section in plugin.settings_schema().sections.clone() {
        let result = crate::theme::collapse(
            ui,
            (&id_source, &section.label),
            &section.label,
            section.default_open,
            None,
            |ui| {
                if let Some(description) = &section.description {
                    ui.weak(description);
                }
                let mut section_changed = false;
                for item in &section.items {
                    match render_setting_item(ui, plugin, item, &id_source) {
                        Ok(item_changed) => {
                            if item_changed {
                                section_changed = true;
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
                Ok(section_changed)
            },
        );
        match result {
            Some(Err(err)) => return Err(err),
            Some(Ok(true)) => changed = true,
            _ => {}
        }
    }

    let status_entries = match authoritative_status {
        Some(status) => status.to_vec(),
        None => plugin.status_entries_cached()?,
    };
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
    id_source: &impl std::hash::Hash,
) -> Result<bool, String> {
    let widget_id = (id_source, item.key.as_str());
    match &item.kind {
        SettingKind::Bool { default } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_bool())
                .unwrap_or(*default);
            let response = ui
                .push_id(widget_id, |ui| ui.checkbox(&mut value, &item.label))
                .inner;
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
            crate::theme::field_label(ui, &item.label, suffix.as_deref());
            let mut slider = egui::Slider::new(&mut value, *min..=*max);
            if let Some(sfx) = suffix {
                slider = slider.suffix(sfx.as_str());
            }
            let response = ui.push_id(widget_id, |ui| ui.add(slider)).inner;
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
            crate::theme::field_label(ui, &item.label, suffix.as_deref());
            let mut slider = egui::Slider::new(&mut value, *min..=*max);
            if let Some(sfx) = suffix {
                slider = slider.suffix(sfx.as_str());
            }
            let response = ui.push_id(widget_id, |ui| ui.add(slider)).inner;
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
            let response = ui
                .push_id(widget_id, |ui| {
                    ui.horizontal(|ui| {
                        crate::theme::field_label(ui, &item.label, None);
                        ui.add(
                            egui::DragValue::new(&mut value)
                                .clamp_range(*min..=*max)
                                .speed(*speed),
                        )
                    })
                    .response
                })
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
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
            let response = ui
                .push_id(widget_id, |ui| {
                    ui.horizontal(|ui| {
                        crate::theme::field_label(ui, &item.label, None);
                        ui.add(egui::DragValue::new(&mut value).clamp_range(*min..=*max))
                    })
                    .response
                })
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
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
            crate::theme::field_label(ui, &item.label, None);
            let response = ui
                .push_id(widget_id, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for (index, variant) in variants.iter().enumerate() {
                            ui.radio_value(&mut value, index, variant);
                        }
                    })
                    .response
                })
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            if value != old_value {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::Text { default } => {
            let mut value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| default.clone());
            crate::theme::field_label(ui, &item.label, None);
            let response = ui
                .push_id(widget_id, |ui| ui.text_edit_singleline(&mut value))
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            // Commit per keystroke: `set_setting_value` invalidates the UI
            // cache, so the widget reads back exactly what was typed instead
            // of an older cached value clobbering the edit.
            if response.changed() {
                return plugin.set_setting_value(&item.key, &json!(value));
            }
        }
        SettingKind::Path { dialog, default } => {
            let value = plugin
                .get_setting_value_cached(&item.key)?
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| default.clone());
            crate::theme::field_label(ui, &item.label, None);
            let mut picked: Option<String> = None;
            let response = ui
                .push_id(widget_id, |ui| {
                    ui.horizontal(|ui| {
                        let shown = if value.is_empty() {
                            "(not set)"
                        } else {
                            &value
                        };
                        ui.label(egui::RichText::new(shown).monospace().small());
                        if ui.button("Browse…").clicked() {
                            let file_dialog = rfd::FileDialog::new();
                            let selection = match dialog {
                                augur_plugin_api::PathDialogKind::OpenFile => {
                                    file_dialog.pick_file()
                                }
                                augur_plugin_api::PathDialogKind::SaveFile => {
                                    file_dialog.save_file()
                                }
                                augur_plugin_api::PathDialogKind::Directory => {
                                    file_dialog.pick_folder()
                                }
                            };
                            if let Some(path) = selection {
                                picked = Some(path.display().to_string());
                            }
                        }
                    })
                    .response
                })
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            if let Some(path) = picked {
                return plugin.set_setting_value(&item.key, &json!(path));
            }
        }
        SettingKind::Button { enabled } => {
            let response = ui
                .push_id(widget_id, |ui| {
                    ui.add_enabled(*enabled, egui::Button::new(&item.label))
                })
                .inner;
            maybe_add_tooltip(&response, item.tooltip.as_deref());
            // Momentary trigger: one `set_setting(key, true)` per click, the
            // frame-independent action channel (works with no camera).
            if response.clicked() {
                return plugin.set_setting_value(&item.key, &json!(true));
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
    let palette = crate::theme::palette_for_visuals(ui.visuals());
    let radius = crate::theme::radius::R_3;
    painter.rect_filled(rect, radius, palette.bg_2);
    painter.rect_stroke(rect, radius, egui::Stroke::new(1.0, palette.line));

    if values.len() < 2 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Collecting history",
            egui::FontId::proportional(12.0),
            palette.fg_3,
        );
        return;
    }

    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1e-9);
    let width = rect.width().max(1.0);
    let height = rect.height().max(1.0);
    let stroke_color = if lower_is_better {
        crate::theme::ROI_AMBER
    } else {
        crate::theme::ROI_CYAN
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
