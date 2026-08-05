use augur_core::{
    camera::BiasReadback,
    config::CameraConfig,
    pipeline::{PipelineStatsSnapshot, SensorMonitoringSnapshot},
};

pub(crate) const IMX636_DEM_SLOTS: usize = 64;

/// Beyond this age a monitoring reading is called out as stale rather than
/// shown as if it were current. The control thread refreshes every 500 ms, so
/// anything past this means polling has stalled.
pub(crate) const MONITORING_STALE_AFTER_S: f64 = 2.0;

const BIAS_CODE_TOOLTIP: &str = "Absolute 8-bit bias code programmed on the sensor, next to this unit's factory-trimmed default (the code for offset 0). The IMX636 publishes no conversion from this bias to a physical unit, so the code is the absolute value the sensor actually works with.";

const DEAD_TIME_TOOLTIP: &str = "Refractory period measured by the sensor's own dead-time monitor — the absolute value this refr offset produces, i.e. the minimum time between two events from one pixel. Read live from the camera, not computed from the slider.";

/// A settings section whose header stays live even when its controls do not.
///
/// Replay and locked recordings present these values as a read-only reference,
/// and a reference nobody can expand is not a reference. Only the body is
/// greyed out, so every section still opens and its values stay legible.
fn section<R>(
    ui: &mut egui::Ui,
    id_source: impl std::hash::Hash,
    label: &str,
    default_open: bool,
    right: Option<&str>,
    read_only: bool,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    crate::theme::collapse(ui, id_source, label, default_open, right, |ui| {
        ui.add_enabled_ui(!read_only, body).inner
    })
}

// A UI aggregator that threads scattered app state into one settings panel;
// grouping the arguments would only move the plumbing elsewhere.
#[allow(clippy::too_many_arguments)]
pub fn draw_settings(
    ui: &mut egui::Ui,
    cfg: &mut CameraConfig,
    mask_x: &mut u16,
    mask_y: &mut u16,
    mask_file: &mut String,
    sensor_width: u16,
    sensor_height: u16,
    pipeline_stats: Option<PipelineStatsSnapshot>,
    sensor_monitoring: Option<&SensorMonitoringSnapshot>,
    read_only: bool,
) -> bool {
    let mut changed = false;

    // Absolute values sit next to the offsets that produced them, so a reading
    // that has stopped refreshing must not keep sitting there as if it were
    // current. The Sensor Readout section below explains why it went missing.
    let fresh_monitoring =
        sensor_monitoring.filter(|snapshot| snapshot.age_s <= MONITORING_STALE_AFTER_S);
    let bias_codes = fresh_monitoring.and_then(|snapshot| snapshot.values.biases);

    const BIAS_SLIDER_COUNT: usize = 5;
    let bias_count = format!("{BIAS_SLIDER_COUNT} / {BIAS_SLIDER_COUNT}");
    section(
        ui,
        "settings_biases",
        "Biases",
        false,
        Some(&bias_count),
        read_only,
        |ui| {
            ui.weak("Analog pixel tuning. Values are relative offsets from factory defaults.");
            if bias_codes.is_none() {
                ui.small(match sensor_monitoring {
                    None => {
                        "Absolute codes and the measured dead time appear while the camera runs."
                    }
                    Some(_) => {
                        "Sensor readback stalled — absolute values hidden until it recovers."
                    }
                });
            }

            crate::theme::field_label(ui, "diff_on", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.diff_on, -85..=140))
                .on_hover_text("ON contrast threshold. Lower = more sensitive to brightness increases (more ON events, more noise). Higher = requires larger brightness change.")
                .changed();
            bias_code_readout(ui, bias_codes, |readback| {
                (readback.current.diff_on, readback.factory_default.diff_on)
            });

            crate::theme::field_label(ui, "diff_off", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.diff_off, -35..=190))
                .on_hover_text("OFF contrast threshold. Lower = more sensitive to brightness decreases (more OFF events, more noise). Higher = requires larger dimming change.")
                .changed();
            bias_code_readout(ui, bias_codes, |readback| {
                (readback.current.diff_off, readback.factory_default.diff_off)
            });

            crate::theme::field_label(ui, "fo", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.fo, -35..=55))
                .on_hover_text("Pixel low-pass filter cutoff. Lower = filters more high-frequency flicker (e.g. fluorescent lights) but increases latency. Higher = faster response but admits more flicker noise.")
                .changed();
            bias_code_readout(ui, bias_codes, |readback| {
                (readback.current.fo, readback.factory_default.fo)
            });

            crate::theme::field_label(ui, "hpf", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.hpf, 0..=120))
                .on_hover_text("Pixel high-pass filter cutoff. Lower = responds to slower illumination changes. Higher = only responds to fast transients, filtering out slow changes.")
                .changed();
            bias_code_readout(ui, bias_codes, |readback| {
                (readback.current.hpf, readback.factory_default.hpf)
            });

            crate::theme::field_label(ui, "refr", None);
            changed |= ui
                .add(egui::Slider::new(&mut cfg.biases.refr, -20..=235))
                .on_hover_text("Refractory period. Higher = shorter dead time, allowing faster event rates. Lower = longer dead time, suppresses hot pixel noise but may miss rapid changes.")
                .changed();
            bias_code_readout(ui, bias_codes, |readback| {
                (readback.current.refr, readback.factory_default.refr)
            });
            // The one abstract bias the sensor can report back in a physical
            // unit, so it belongs directly under its slider.
            if let Some(dead_time_us) =
                fresh_monitoring.and_then(|snapshot| snapshot.values.pixel_dead_time_us)
            {
                readout_line(
                    ui,
                    format!("dead time  {}", format_measurement(dead_time_us, "µs")),
                )
                .on_hover_text(DEAD_TIME_TOOLTIP);
            }
        },
    );

    ui.separator();
    // Always present and open by default. Illumination and die temperature are
    // the conditions a bias setting only makes sense against, so they must be
    // readable at a glance rather than behind a collapsed section that appears
    // and disappears with the camera.
    section(
        ui,
        "settings_sensor_readout",
        "Sensor Readout",
        true,
        None,
        read_only,
        |ui| {
            // No standing preamble: with readings present the rows and their
            // hover text say everything, and with none the single line below
            // says the only thing worth saying. Two grey paragraphs used to
            // fill the section while showing no numbers at all.
            match sensor_monitoring {
                Some(snapshot) => draw_sensor_readout(ui, snapshot),
                None => {
                    ui.small(
                        "Illumination, die temperature and dead time appear while the camera runs.",
                    );
                }
            }
        },
    );

    ui.separator();
    section(ui, "settings_roi", "ROI", false, None, read_only, |ui| {
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
    section(
        ui,
        "settings_pixel_mask",
        "Pixel Mask (DEM)",
        false,
        Some(&mask_count),
        read_only,
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
    section(
        ui,
        "settings_external_triggers",
        "External Triggers",
        false,
        None,
        read_only,
        |ui| {
            ui.weak(
                "EVK4 TRIG_IN monitoring. Channel 0 is the only supported input in this release.",
            );
            changed |= ui
                .checkbox(
                    &mut cfg.external_triggers.enabled,
                    "Enable channel 0 (TRIG_IN)",
                )
                .on_hover_text("Inserts EXT_TRIGGER events into the EVT3 stream when an edge arrives on the EVK4 TRIG_IN pin, timestamped on the camera clock. Cannot change while streaming.")
                .changed();

            // Live decode-level edge counter. Counts every decoded edge (rising
            // and falling) before frame windowing and the rising-only filter
            // plugins apply, so a stuck-at-zero count points upstream (wiring,
            // TRIG_IN levels, enable-while-streaming) rather than at the plugin.
            if let Some(stats) = pipeline_stats {
                ui.separator();
                if stats.triggers_total == 0 {
                    ui.small("No trigger edges decoded yet.");
                } else {
                    let last = match stats.last_trigger_age_s {
                        Some(age) if age < 1.0 => "last <1 s ago".to_string(),
                        Some(age) => format!("last {age:.1} s ago"),
                        None => "last —".to_string(),
                    };
                    ui.small(format!("Edges decoded: {} · {last}", stats.triggers_total));
                    if stats.triggers_dropped > 0 {
                        ui.small(
                            egui::RichText::new(format!(
                                "{} pending edges dropped",
                                stats.triggers_dropped
                            ))
                            .color(ui.visuals().warn_fg_color),
                        )
                        .on_hover_text(
                            "Trigger edges are attached to preview frames, which only close on CD \
                             events. With too few CD events to close a window the pending buffer \
                             filled and the oldest edges were discarded. Recorded RAW data and \
                             analysis runs are unaffected.",
                        );
                    }
                }
            }
        },
    );

    ui.separator();
    section(
        ui,
        "settings_filters",
        "Digital Filters",
        false,
        None,
        read_only,
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

/// Two-column sensor row: label left, measured value right-aligned. Hovering
/// anywhere on the row explains what the sensor actually measured.
fn readout_row(ui: &mut egui::Ui, label: &str, value: &str) -> egui::Response {
    crate::theme::inspector_row(ui, label, value)
}

/// Muted monospace line used for the absolute readouts under a slider.
fn readout_line(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let palette = crate::theme::palette_for_visuals(ui.visuals());
    ui.label(
        egui::RichText::new(text.into())
            .monospace()
            .size(11.0)
            .color(palette.fg_3),
    )
}

/// The absolute bias code programmed on the sensor plus this unit's factory
/// default, drawn under the slider holding the relative offset.
fn bias_code_readout(
    ui: &mut egui::Ui,
    readback: Option<BiasReadback>,
    pick: impl FnOnce(&BiasReadback) -> (u8, u8),
) {
    let Some(readback) = readback else {
        return;
    };
    let (code, factory_default) = pick(&readback);
    readout_line(ui, format!("abs {code}  ·  factory {factory_default}"))
        .on_hover_text(BIAS_CODE_TOOLTIP);
}

/// Sensor-measured quantities that are not settings themselves but describe the
/// conditions a bias setting was chosen under.
fn draw_sensor_readout(ui: &mut egui::Ui, snapshot: &SensorMonitoringSnapshot) {
    let values = &snapshot.values;
    let mut any_reading = false;

    // Two-column rows rather than space-padded monospace: the values line up
    // on the right edge whatever their width, and stay aligned if a label or
    // a unit ever changes length.
    if let Some(dead_time_us) = values.pixel_dead_time_us {
        readout_row(ui, "dead time", &format_measurement(dead_time_us, "µs"))
            .on_hover_text(DEAD_TIME_TOOLTIP);
        any_reading = true;
    }
    if let Some(lux) = values.illumination_lux {
        readout_row(ui, "illumination", &format_measurement(lux, "lux"))
            .on_hover_text("Scene illumination integrated by the sensor's LIFO block. Worth recording next to a bias setting, since the usable bias range depends on the light level.");
        any_reading = true;
    }
    if let Some(temperature_c) = values.temperature_c {
        readout_row(ui, "die temp", &format!("{temperature_c:.1} °C"))
            .on_hover_text("Sensor die temperature from the on-chip ADC. Analog bias behaviour drifts with temperature, so this belongs in the log of a bias sweep.");
        any_reading = true;
    }
    if !any_reading {
        ui.small("No measurement available right now.");
    }

    if snapshot.age_s > MONITORING_STALE_AFTER_S {
        ui.small(
            egui::RichText::new(format!("last update {:.0} s ago", snapshot.age_s))
                .color(ui.visuals().warn_fg_color),
        )
        .on_hover_text(
            "Readings normally refresh twice per second. A growing age means the camera control \
             thread is no longer completing the readback.",
        );
    }
    if let Some(error) = &snapshot.error {
        ui.small(egui::RichText::new(error).color(ui.visuals().warn_fg_color))
            .on_hover_text(
                "The most recent readback failed. Any values above are the last successful reading.",
            );
    }
}

/// Three significant digits, so a value that jitters in its last decimal does
/// not make the panel look unstable.
pub(crate) fn format_measurement(value: f32, unit: &str) -> String {
    if value >= 100.0 {
        format!("{value:.0} {unit}")
    } else if value >= 10.0 {
        format!("{value:.1} {unit}")
    } else {
        format!("{value:.2} {unit}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measurement_precision_shrinks_with_magnitude() {
        assert_eq!(format_measurement(6.3456, "µs"), "6.35 µs");
        assert_eq!(format_measurement(42.4242, "µs"), "42.4 µs");
        assert_eq!(format_measurement(1234.7, "lux"), "1235 lux");
    }

    #[test]
    fn measurement_keeps_two_decimals_below_ten() {
        assert_eq!(format_measurement(0.0, "lux"), "0.00 lux");
        assert_eq!(format_measurement(9.999, "µs"), "10.00 µs");
    }
}
