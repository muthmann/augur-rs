use std::path::{Path, PathBuf};

use augur_core::{config::RoiConfig, replay::ReplayFileInfo};

use crate::{
    app::ensure_extension,
    export::{estimate_tiff_stack_frames, TiffStackExportParams},
};

#[derive(Debug)]
pub(crate) enum ExportDialogAction {
    Export(TiffStackExportParams),
    Cancel,
}

#[derive(Debug, Clone)]
pub(crate) struct ExportDialog {
    pub(crate) open: bool,
    exporting: bool,
    total_duration_us: u64,
    first_timestamp_us: u64,
    width: u16,
    height: u16,
    current_roi: RoiConfig,
    start_seconds: f64,
    end_seconds: f64,
    acq_time_ms: u64,
    crop_to_roi: bool,
    output_path: String,
    /// The native save dialog already asked about overwriting the path it
    /// returned, so re-asking would be noise. Any manual edit clears this and
    /// the in-dialog confirmation takes over.
    overwrite_confirmed: bool,
    pending_overwrite: Option<TiffStackExportParams>,
    error: Option<String>,
    info: Option<String>,
}

impl Default for ExportDialog {
    fn default() -> Self {
        Self {
            open: false,
            exporting: false,
            total_duration_us: 0,
            first_timestamp_us: 0,
            width: 0,
            height: 0,
            current_roi: RoiConfig::full_frame(),
            start_seconds: 0.0,
            end_seconds: 0.0,
            acq_time_ms: 50,
            crop_to_roi: false,
            output_path: String::new(),
            overwrite_confirmed: false,
            pending_overwrite: None,
            error: None,
            info: None,
        }
    }
}

impl ExportDialog {
    pub(crate) fn open_for_replay(
        &mut self,
        replay_path: &Path,
        info: &ReplayFileInfo,
        acq_time_ms: u64,
        roi: RoiConfig,
    ) {
        let effective_duration_us = info.total_duration_us.max(acq_time_ms.max(1) * 1_000);
        self.open = true;
        self.exporting = false;
        self.total_duration_us = effective_duration_us;
        self.first_timestamp_us = info.first_timestamp_us;
        self.width = info.width;
        self.height = info.height;
        self.current_roi = roi;
        self.start_seconds = 0.0;
        self.end_seconds = effective_duration_us as f64 / 1_000_000.0;
        self.acq_time_ms = acq_time_ms.max(1);
        self.crop_to_roi = !roi_matches_full_frame(roi, info.width, info.height);
        self.output_path = default_export_path(replay_path).display().to_string();
        self.overwrite_confirmed = false;
        self.pending_overwrite = None;
        self.error = None;
        self.info = None;
    }

    pub(crate) fn set_exporting(&mut self, exporting: bool) {
        self.exporting = exporting;
        if exporting {
            self.error = None;
            self.info = None;
            self.pending_overwrite = None;
        }
    }

    pub(crate) fn finish_success(&mut self, frame_count: usize, output_path: &Path) {
        self.exporting = false;
        self.info = Some(format!(
            "Exported {frame_count} frame(s) to {}",
            output_path.display()
        ));
        self.error = None;
    }

    pub(crate) fn finish_error(&mut self, message: String) {
        self.exporting = false;
        self.error = Some(message);
        self.info = None;
    }

    pub(crate) fn show(&mut self, ctx: &egui::Context) -> Option<ExportDialogAction> {
        if !self.open {
            return None;
        }

        let total_duration_s = self.total_duration_us as f64 / 1_000_000.0;
        self.start_seconds = self.start_seconds.clamp(0.0, total_duration_s);
        self.end_seconds = self.end_seconds.clamp(self.start_seconds, total_duration_s);
        self.acq_time_ms = self.acq_time_ms.max(1);

        let mut action = None;
        let mut open = self.open;
        let mut window = egui::Window::new("Export TIFF Stack")
            .collapsible(false)
            .resizable(false);
        // A running export cannot be stopped, so there is nothing for a close
        // button to mean except "hide the truth". Withdraw it until the export
        // finishes.
        if !self.exporting {
            window = window.open(&mut open);
        }
        window.show(ctx, |ui| {
            ui.label("Batch export replay frames as a multi-page 16-bit grayscale TIFF.");
            ui.small("Each page stores total ON+OFF counts for one accumulation window.");
            ui.separator();

            ui.add_enabled_ui(!self.exporting, |ui| {
                ui.label("Time range [s]");
                let start_response = ui.add(
                    egui::Slider::new(&mut self.start_seconds, 0.0..=total_duration_s)
                        .text("Start")
                        .fixed_decimals(3),
                );
                if start_response.changed() {
                    self.end_seconds = self.end_seconds.max(self.start_seconds);
                }
                ui.add(
                    egui::Slider::new(&mut self.end_seconds, self.start_seconds..=total_duration_s)
                        .text("End")
                        .fixed_decimals(3),
                );

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Acquisition time [ms]");
                    ui.add(egui::DragValue::new(&mut self.acq_time_ms).range(1..=60_000));
                });

                let roi_label = format!(
                    "Crop to current ROI ({},{} {}x{})",
                    self.current_roi.x,
                    self.current_roi.y,
                    self.current_roi.width,
                    self.current_roi.height
                );
                ui.checkbox(&mut self.crop_to_roi, roi_label);

                ui.separator();
                ui.label("Output path");
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::TextEdit::singleline(&mut self.output_path).desired_width(360.0))
                        .changed()
                    {
                        // A hand-typed path has not been confirmed by
                        // anyone yet.
                        self.overwrite_confirmed = false;
                        self.pending_overwrite = None;
                    }
                    if ui.button("Browse…").clicked() {
                        let file_name = Path::new(self.output_path.trim())
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .filter(|name| !name.is_empty())
                            .unwrap_or_else(|| "replay_stack.tiff".into());
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name(&file_name)
                            .add_filter("TIFF", &["tif", "tiff"])
                            .save_file()
                        {
                            self.output_path = path.display().to_string();
                            // The native dialog already handled the
                            // overwrite prompt for this exact path.
                            self.overwrite_confirmed = true;
                            self.pending_overwrite = None;
                        }
                    }
                });
            });

            let estimated_frames = estimate_tiff_stack_frames(
                seconds_to_us(self.start_seconds),
                seconds_to_us(self.end_seconds),
                self.acq_time_ms.saturating_mul(1_000),
            );
            ui.small(format!("Estimated frame count: {estimated_frames}"));

            if self.exporting {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Exporting TIFF stack...");
                });
                // The window's close button is withdrawn below while this
                // runs, so the state on screen matches the state on disk.
                ui.small(
                    "The export writes on a background thread and cannot be \
                         interrupted. This window stays open until it finishes.",
                );
            }
            if let Some(pending) = &self.pending_overwrite {
                ui.separator();
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("{} already exists.", pending.output_path.display()),
                );
                ui.small("Exporting replaces the whole file; the current contents are lost.");
                ui.horizontal(|ui| {
                    if ui.button("Overwrite").clicked() {
                        self.overwrite_confirmed = true;
                        if let Some(params) = self.pending_overwrite.take() {
                            self.error = None;
                            self.info = None;
                            action = Some(ExportDialogAction::Export(params));
                        }
                    }
                    if ui.button("Keep existing file").clicked() {
                        self.pending_overwrite = None;
                    }
                });
            }
            if let Some(info) = &self.info {
                ui.colored_label(egui::Color32::from_rgb(40, 140, 90), info);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }

            ui.separator();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!self.exporting, egui::Button::new("Export"))
                    .clicked()
                {
                    match self.build_export_params() {
                        // Writing the stack truncates the target, so an
                        // existing file the user typed in by hand is only
                        // replaced after they say so.
                        Ok(params) if !self.overwrite_confirmed && params.output_path.exists() => {
                            self.error = None;
                            self.info = None;
                            self.pending_overwrite = Some(params);
                        }
                        Ok(params) => {
                            self.error = None;
                            self.info = None;
                            self.pending_overwrite = None;
                            action = Some(ExportDialogAction::Export(params));
                        }
                        Err(err) => self.error = Some(err),
                    }
                }
                if ui
                    .add_enabled(!self.exporting, egui::Button::new("Cancel"))
                    .clicked()
                {
                    action = Some(ExportDialogAction::Cancel);
                    self.open = false;
                }
            });
        });
        if self.exporting {
            // `open` was never handed to the window this frame; keep it true so
            // the dialog does not disappear out from under a running export.
            open = true;
        }

        self.open = open;
        if !self.open && action.is_none() {
            return Some(ExportDialogAction::Cancel);
        }

        action
    }

    fn build_export_params(&self) -> Result<TiffStackExportParams, String> {
        let output_path = self.output_path.trim();
        if output_path.is_empty() {
            return Err("choose an output path".into());
        }

        let start_offset_us = seconds_to_us(self.start_seconds);
        let end_offset_us = seconds_to_us(self.end_seconds);
        if end_offset_us <= start_offset_us {
            return Err("end time must be greater than start time".into());
        }

        Ok(TiffStackExportParams {
            acq_time_us: self.acq_time_ms.saturating_mul(1_000),
            start_us: self.first_timestamp_us.saturating_add(start_offset_us),
            end_us: self.first_timestamp_us.saturating_add(end_offset_us),
            roi: self.crop_to_roi.then_some(self.current_roi),
            // Resolve the extension here, not at dispatch: the overwrite check
            // and the confirmation text must name the file that actually gets
            // written.
            output_path: ensure_extension(PathBuf::from(output_path), "tiff"),
            width: self.width,
            height: self.height,
        })
    }
}

fn roi_matches_full_frame(roi: RoiConfig, width: u16, height: u16) -> bool {
    roi.x == 0 && roi.y == 0 && roi.width == width && roi.height == height
}

fn default_export_path(replay_path: &Path) -> PathBuf {
    let stem = replay_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "replay".into());
    replay_path.with_file_name(format!("{stem}_stack.tiff"))
}

fn seconds_to_us(seconds: f64) -> u64 {
    (seconds.max(0.0) * 1_000_000.0).round() as u64
}

#[cfg(test)]
mod tests {
    use super::ExportDialog;

    fn dialog_for(output_path: &str) -> ExportDialog {
        ExportDialog {
            output_path: output_path.to_owned(),
            end_seconds: 1.0,
            width: 4,
            height: 4,
            ..ExportDialog::default()
        }
    }

    #[test]
    fn params_name_the_file_that_will_actually_be_written() {
        // The exporter forces a .tiff extension, so the overwrite check and the
        // confirmation text must see the same resolved path — not the stem the
        // user typed.
        let params = dialog_for("/tmp/stack")
            .build_export_params()
            .expect("params build");
        assert_eq!(params.output_path.to_string_lossy(), "/tmp/stack.tiff");

        let params = dialog_for("/tmp/stack.tif")
            .build_export_params()
            .expect("params build");
        assert_eq!(params.output_path.to_string_lossy(), "/tmp/stack.tif");
    }

    #[test]
    fn a_hand_typed_path_starts_unconfirmed_for_overwriting() {
        let dialog = dialog_for("/tmp/stack.tiff");
        assert!(!dialog.overwrite_confirmed);
        assert!(dialog.pending_overwrite.is_none());
    }
}
