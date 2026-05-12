use augur_core::config::CameraConfig;

pub(crate) const IMX636_DEM_SLOTS: usize = 64;

pub fn draw_settings(
    ui: &mut egui::Ui,
    cfg: &mut CameraConfig,
    mask_x: &mut u16,
    mask_y: &mut u16,
    mask_file: &mut String,
    sensor_width: u16,
    sensor_height: u16,
) -> bool {
    let mut changed = false;

    const BIAS_SLIDER_COUNT: usize = 5;
    let bias_count = format!("{BIAS_SLIDER_COUNT} / {BIAS_SLIDER_COUNT}");
    crate::theme::collapse(
        ui,
        "settings_biases",
        "Biases",
        false,
        Some(&bias_count),
        |ui| {
            ui.weak("Analog pixel tuning. Values are relative offsets from factory defaults.");

            crate::theme::field_label(ui, "diff_on", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.diff_on, -85..=140))
                .on_hover_text("ON contrast threshold. Lower = more sensitive to brightness increases (more ON events, more noise). Higher = requires larger brightness change.")
                .changed();

            crate::theme::field_label(ui, "diff_off", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.diff_off, -35..=190))
                .on_hover_text("OFF contrast threshold. Lower = more sensitive to brightness decreases (more OFF events, more noise). Higher = requires larger dimming change.")
                .changed();

            crate::theme::field_label(ui, "fo", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.fo, -35..=55))
                .on_hover_text("Pixel low-pass filter cutoff. Lower = filters more high-frequency flicker (e.g. fluorescent lights) but increases latency. Higher = faster response but admits more flicker noise.")
                .changed();

            crate::theme::field_label(ui, "hpf", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.hpf, 0..=120))
                .on_hover_text("Pixel high-pass filter cutoff. Lower = responds to slower illumination changes. Higher = only responds to fast transients, filtering out slow changes.")
                .changed();

            crate::theme::field_label(ui, "refr", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.refr, -20..=235))
                .on_hover_text("Refractory period. Higher = shorter dead time, allowing faster event rates. Lower = longer dead time, suppresses hot pixel noise but may miss rapid changes.")
                .changed();
        },
    );

    ui.separator();
    crate::theme::collapse(ui, "settings_roi", "ROI", false, None, |ui| {
        ui.weak("Hardware Region of Interest. Only pixels inside this rectangle are active. Inactive pixels consume no power and produce no events.");
        crate::theme::field_label(ui, "Rect", Some("px"));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = crate::theme::sp::SP_2;
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.x)
                        .prefix("x ")
                        .clamp_range(0..=sensor_width.saturating_sub(1)),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.y)
                        .prefix("y ")
                        .clamp_range(0..=sensor_height.saturating_sub(1)),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.width)
                        .prefix("w ")
                        .clamp_range(1..=sensor_width.max(1)),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.height)
                        .prefix("h ")
                        .clamp_range(1..=sensor_height.max(1)),
                )
                .changed();
        });
    });

    ui.separator();
    let mask_count = format!(
        "{} / {}",
        cfg.pixel_mask.masked_pixels.len(),
        IMX636_DEM_SLOTS
    );
    crate::theme::collapse(
        ui,
        "settings_pixel_mask",
        "Pixel Mask (DEM)",
        false,
        Some(&mask_count),
        |ui| {
            ui.weak("Digital Event Mask. Suppresses events from up to 64 individual defective or hot pixels in hardware. Use this to silence persistently noisy pixels.");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::sp::SP_2;
                ui.add(
                    egui::DragValue::new(mask_x)
                        .prefix("x ")
                        .clamp_range(0..=sensor_width.saturating_sub(1)),
                );
                ui.add(
                    egui::DragValue::new(mask_y)
                        .prefix("y ")
                        .clamp_range(0..=sensor_height.saturating_sub(1)),
                );
                let can_add = cfg.pixel_mask.masked_pixels.len() < IMX636_DEM_SLOTS;
                if ui
                    .add_enabled(can_add, egui::Button::new("Add pixel"))
                    .clicked()
                {
                    let p = (*mask_x, *mask_y);
                    if !cfg.pixel_mask.masked_pixels.contains(&p) {
                        cfg.pixel_mask.masked_pixels.push(p);
                        changed = true;
                    }
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = crate::theme::sp::SP_2;
                ui.label("mask file");
                ui.add(
                    egui::TextEdit::singleline(mask_file)
                        .desired_width(ui.available_width() - 80.0),
                );
                if ui.button("Browse\u{2026}").clicked() {
                    if let Some(path) = rfd::FileDialog::new().pick_file() {
                        *mask_file = path.display().to_string();
                    }
                }
            });
            if ui.button("Use mask file").clicked() {
                cfg.pixel_mask.mask_file = if mask_file.trim().is_empty() {
                    None
                } else {
                    Some(mask_file.trim().into())
                };
                changed = true;
            }

            egui::ScrollArea::vertical()
                .max_height(140.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut remove_idx = None;
                    for (idx, (x, y)) in cfg.pixel_mask.masked_pixels.iter().copied().enumerate() {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("#{idx:02}  ({x}, {y})"));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("Remove").clicked() {
                                        remove_idx = Some(idx);
                                    }
                                },
                            );
                        });
                    }
                    if let Some(idx) = remove_idx {
                        cfg.pixel_mask.masked_pixels.remove(idx);
                        changed = true;
                    }
                });

            if cfg.pixel_mask.masked_pixels.is_empty() {
                ui.small("No masked pixels configured.");
            }

            if ui.button("Clear masked pixels").clicked() {
                cfg.pixel_mask.masked_pixels.clear();
                changed = true;
            }
        },
    );

    ui.separator();
    crate::theme::collapse(
        ui,
        "settings_filters",
        "Digital Filters",
        false,
        None,
        |ui| {
            ui.weak("Hardware noise filters on the sensor's digital pipeline. STC and Trail are mutually exclusive — they share the same on-chip processing block.");
            if ui
                .checkbox(&mut cfg.digital_filter.stc_enabled, "STC enabled")
                .on_hover_text("Spatio-Temporal Contrast filter. Suppresses isolated noise events by requiring a confirming second event within the threshold window. Keeps the second event of a burst.")
                .changed()
            {
                changed = true;
                if cfg.digital_filter.stc_enabled {
                    cfg.digital_filter.trail_enabled = false;
                }
            }
            crate::theme::field_label(ui, "STC threshold", Some("µs"));
            changed |= ui
                .add(egui::Slider::new(
                    &mut cfg.digital_filter.stc_threshold_us,
                    1_000..=100_000,
                ))
                .on_hover_text("Maximum time window (in microseconds) within which successive same-polarity events are considered part of a burst. Lower = stricter filtering.")
                .changed();
            if ui
                .checkbox(&mut cfg.digital_filter.trail_enabled, "Trail enabled")
                .on_hover_text("Trail filter. Keeps only the first event after a polarity transition and suppresses redundant trailing events of the same polarity within the threshold window.")
                .changed()
            {
                changed = true;
                if cfg.digital_filter.trail_enabled {
                    cfg.digital_filter.stc_enabled = false;
                }
            }
        },
    );

    changed
}
