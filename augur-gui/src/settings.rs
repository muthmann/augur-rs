use augur_core::config::CameraConfig;

const SENSOR_WIDTH: u16 = 1280;
const SENSOR_HEIGHT: u16 = 720;
pub(crate) const IMX636_DEM_SLOTS: usize = 64;

pub fn draw_settings(
    ui: &mut egui::Ui,
    cfg: &mut CameraConfig,
    mask_x: &mut u16,
    mask_y: &mut u16,
    mask_file: &mut String,
) -> bool {
    let mut changed = false;

    ui.collapsing("Biases", |ui| {
        ui.weak("Analog pixel tuning. Values are relative offsets from factory defaults.");

        changed |= ui
            .add(egui::Slider::new(&mut cfg.biases.diff_on, -85..=140).text("diff_on"))
            .on_hover_text("ON contrast threshold. Lower = more sensitive to brightness increases (more ON events, more noise). Higher = requires larger brightness change.")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.biases.diff_off, -35..=190).text("diff_off"))
            .on_hover_text("OFF contrast threshold. Lower = more sensitive to brightness decreases (more OFF events, more noise). Higher = requires larger dimming change.")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.biases.fo, -35..=55).text("fo"))
            .on_hover_text("Pixel low-pass filter cutoff. Lower = filters more high-frequency flicker (e.g. fluorescent lights) but increases latency. Higher = faster response but admits more flicker noise.")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.biases.hpf, 0..=120).text("hpf"))
            .on_hover_text("Pixel high-pass filter cutoff. Lower = responds to slower illumination changes. Higher = only responds to fast transients, filtering out slow changes.")
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut cfg.biases.refr, -20..=235).text("refr"))
            .on_hover_text("Refractory period. Higher = shorter dead time, allowing faster event rates. Lower = longer dead time, suppresses hot pixel noise but may miss rapid changes.")
            .changed();
    });

    ui.separator();
    ui.collapsing("ROI", |ui| {
        ui.weak("Hardware Region of Interest. Only pixels inside this rectangle are active. Inactive pixels consume no power and produce no events.");
        ui.horizontal(|ui| {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.x)
                        .prefix("x ")
                        .clamp_range(0..=SENSOR_WIDTH - 1),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.y)
                        .prefix("y ")
                        .clamp_range(0..=SENSOR_HEIGHT - 1),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.width)
                        .prefix("w ")
                        .clamp_range(1..=SENSOR_WIDTH),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut cfg.roi.height)
                        .prefix("h ")
                        .clamp_range(1..=SENSOR_HEIGHT),
                )
                .changed();
        });
    });

    ui.separator();
    ui.collapsing("Pixel Mask (DEM)", |ui| {
        ui.weak("Digital Event Mask. Suppresses events from up to 64 individual defective or hot pixels in hardware. Use this to silence persistently noisy pixels.");
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(mask_x)
                    .prefix("x ")
                    .clamp_range(0..=SENSOR_WIDTH - 1),
            );
            ui.add(
                egui::DragValue::new(mask_y)
                    .prefix("y ")
                    .clamp_range(0..=SENSOR_HEIGHT - 1),
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

        ui.horizontal(|ui| {
            ui.label("mask file");
            ui.text_edit_singleline(mask_file);
            if ui.button("Browse…").clicked() {
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

        ui.label(format!(
            "masked pixels: {} / {}",
            cfg.pixel_mask.masked_pixels.len(),
            IMX636_DEM_SLOTS
        ));

        egui::ScrollArea::vertical()
            .max_height(140.0)
            .show(ui, |ui| {
                let mut remove_idx = None;
                for (idx, (x, y)) in cfg.pixel_mask.masked_pixels.iter().copied().enumerate() {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("#{idx:02}  ({x}, {y})"));
                        if ui.small_button("Remove").clicked() {
                            remove_idx = Some(idx);
                        }
                    });
                }
                if let Some(idx) = remove_idx {
                    cfg.pixel_mask.masked_pixels.remove(idx);
                    changed = true;
                }
            });

        if cfg.pixel_mask.masked_pixels.is_empty() {
            ui.label("No masked pixels configured.");
        }

        if ui.button("Clear masked pixels").clicked() {
            cfg.pixel_mask.masked_pixels.clear();
            changed = true;
        }
    });

    ui.separator();
    ui.collapsing("Digital Filters", |ui| {
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
        changed |= ui
            .add(
                egui::Slider::new(&mut cfg.digital_filter.stc_threshold_us, 1_000..=100_000)
                    .text("STC threshold [us]"),
            )
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
    });

    changed
}
