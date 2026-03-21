use std::sync::Arc;

use augur_core::{
    analysis::{
        roi_grid::{self, RoiGrid},
        AnalysisOutput, Overlay,
    },
    config::CameraConfig,
    pipeline::PreviewFrame,
};

use crate::plugin::AnalysisPlugin;

pub struct RoiGridPlugin {
    enabled: bool,
    roi_grid: Option<Arc<RoiGrid>>,
    show_roi_grid: bool,
    roi_grid_top_n: usize,
    last_mask_snapshot: Vec<(u16, u16)>,
}

impl Default for RoiGridPlugin {
    fn default() -> Self {
        Self {
            enabled: true,
            roi_grid: None,
            show_roi_grid: false,
            roi_grid_top_n: 3,
            last_mask_snapshot: Vec::new(),
        }
    }
}

impl RoiGridPlugin {
    fn recompute_roi_grid(&mut self, config: &CameraConfig, sensor_width: u16, sensor_height: u16) {
        let grid = roi_grid::compute_roi_grid(
            &config.pixel_mask.masked_pixels,
            sensor_width,
            sensor_height,
            self.roi_grid_top_n.max(1),
        );
        self.last_mask_snapshot = config.pixel_mask.masked_pixels.clone();
        self.roi_grid = Some(Arc::new(grid));
    }

    fn maybe_auto_recompute_roi_grid(
        &mut self,
        config: &CameraConfig,
        sensor_width: u16,
        sensor_height: u16,
    ) {
        if config.pixel_mask.masked_pixels == self.last_mask_snapshot {
            return;
        }

        if self.roi_grid.is_some() || self.show_roi_grid {
            self.recompute_roi_grid(config, sensor_width, sensor_height);
        } else {
            self.last_mask_snapshot = config.pixel_mask.masked_pixels.clone();
        }
    }
}

impl AnalysisPlugin for RoiGridPlugin {
    fn name(&self) -> &str {
        "ROI Grid"
    }

    fn description(&self) -> &str {
        "Finds the largest hotpixel-free rectangular regions on the sensor."
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    fn ui_settings(
        &mut self,
        ui: &mut egui::Ui,
        config: &mut CameraConfig,
        sensor_width: u16,
        sensor_height: u16,
    ) -> bool {
        self.maybe_auto_recompute_roi_grid(config, sensor_width, sensor_height);

        let mut config_changed = false;

        egui::CollapsingHeader::new(self.name())
            .default_open(true)
            .show(ui, |ui| {
                ui.weak(self.description());
                if ui.button("Compute ROI Grid").clicked() {
                    self.recompute_roi_grid(config, sensor_width, sensor_height);
                    self.show_roi_grid = true;
                }

                ui.checkbox(&mut self.show_roi_grid, "Show ROI Grid overlay");

                ui.horizontal(|ui| {
                    ui.label("Top N").on_hover_text(
                        "Number of largest free rectangles to show. The biggest ones are the best ROI candidates.",
                    );
                    if ui
                        .add(egui::DragValue::new(&mut self.roi_grid_top_n).clamp_range(1..=10))
                        .changed()
                        && self.roi_grid.is_some()
                    {
                        self.recompute_roi_grid(config, sensor_width, sensor_height);
                    }
                });

                if let Some(grid) = &self.roi_grid {
                    ui.label(format!(
                        "Grid: {}x{} cells, {} free",
                        grid.x_bounds.len() - 1,
                        grid.y_bounds.len() - 1,
                        grid.free_cells.len(),
                    ));

                    if grid.largest_rects.is_empty() {
                        ui.label("No free rectangles found.");
                    } else {
                        ui.label("Largest rectangles:");
                        let mut use_idx = None;
                        egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                            for (i, rect) in grid.largest_rects.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.monospace(format!(
                                        "#{} ({},{}) {}x{} = {} px",
                                        i + 1,
                                        rect.x,
                                        rect.y,
                                        rect.width,
                                        rect.height,
                                        rect.area(),
                                    ));
                                    if ui.small_button("Use as ROI").clicked() {
                                        use_idx = Some(i);
                                    }
                                });
                            }
                        });

                        if let Some(i) = use_idx {
                            let r = &grid.largest_rects[i];
                            config.roi.x = r.x;
                            config.roi.y = r.y;
                            config.roi.width = r.width;
                            config.roi.height = r.height;
                            config_changed = true;
                        }
                    }
                } else {
                    ui.label("Click 'Compute ROI Grid' to analyze masked pixels.");
                }
            });

        config_changed
    }

    fn process_frame(&mut self, _frame: &PreviewFrame, output: &mut AnalysisOutput) {
        if self.show_roi_grid {
            if let Some(grid) = &self.roi_grid {
                output.overlays.push(Overlay::RoiGrid {
                    grid: grid.clone(),
                    highlight_top_n: self.roi_grid_top_n,
                });
            }
        }
    }

    fn reset(&mut self) {
        self.roi_grid = None;
        self.show_roi_grid = false;
        self.last_mask_snapshot.clear();
    }
}
