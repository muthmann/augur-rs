use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use augur_plugin_api::LocalizationTable;
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use image::{ImageFormat, RgbaImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructionColormap {
    Hot,
    Gray,
}

impl ReconstructionColormap {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hot => "Hot",
            Self::Gray => "Gray",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructionImageFormat {
    Png,
    Tiff,
}

impl ReconstructionImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tiff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconstructionRenderSettings {
    pub pixel_size_nm: f64,
    pub contrast_percentile: f32,
    pub colormap: ReconstructionColormap,
}

impl Default for ReconstructionRenderSettings {
    fn default() -> Self {
        Self {
            pixel_size_nm: 10.0,
            contrast_percentile: 99.5,
            colormap: ReconstructionColormap::Hot,
        }
    }
}

#[derive(Default)]
pub struct ReconstructionState {
    table: Option<LocalizationTable>,
    image: Option<ColorImage>,
    texture: Option<TextureHandle>,
    dirty: bool,
    rendered_size: [usize; 2],
}

impl ReconstructionState {
    pub fn clear(&mut self) {
        self.table = None;
        self.image = None;
        self.texture = None;
        self.dirty = false;
        self.rendered_size = [0, 0];
    }

    pub fn set_table(&mut self, table: Option<LocalizationTable>) {
        if self.table == table {
            return;
        }
        self.table = table;
        self.dirty = true;
        if self.table.is_none() {
            self.image = None;
            self.texture = None;
            self.rendered_size = [0, 0];
        }
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn render_if_needed(
        &mut self,
        ctx: &egui::Context,
        settings: ReconstructionRenderSettings,
    ) {
        if !self.dirty {
            return;
        }

        let Some(table) = &self.table else {
            self.image = None;
            self.texture = None;
            self.rendered_size = [0, 0];
            self.dirty = false;
            return;
        };

        let rendered = render_reconstruction(table, settings);
        self.rendered_size = rendered.size;
        self.image = Some(rendered.image.clone());
        if let Some(texture) = &mut self.texture {
            texture.set(rendered.image, TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("reconstruction", rendered.image, TextureOptions::LINEAR));
        }
        self.dirty = false;
    }

    pub fn table(&self) -> Option<&LocalizationTable> {
        self.table.as_ref()
    }

    pub fn image(&self) -> Option<&ColorImage> {
        self.image.as_ref()
    }

    pub fn texture(&self) -> Option<&TextureHandle> {
        self.texture.as_ref()
    }

    pub fn rendered_size(&self) -> [usize; 2] {
        self.rendered_size
    }
}

#[derive(Clone)]
pub struct ReconstructionSharedData {
    pub texture: Option<TextureHandle>,
    pub total_localizations: usize,
    pub rendered_width: usize,
    pub rendered_height: usize,
    pub pixel_size_nm: f64,
    pub contrast_percentile: f32,
    pub colormap: ReconstructionColormap,
    pub zoom: f32,
    pub close_requested: bool,
    pub export_csv_requested: bool,
    pub export_image_requested: Option<ReconstructionImageFormat>,
    pub clear_requested: bool,
}

impl Default for ReconstructionSharedData {
    fn default() -> Self {
        Self {
            texture: None,
            total_localizations: 0,
            rendered_width: 0,
            rendered_height: 0,
            pixel_size_nm: ReconstructionRenderSettings::default().pixel_size_nm,
            contrast_percentile: ReconstructionRenderSettings::default().contrast_percentile,
            colormap: ReconstructionRenderSettings::default().colormap,
            zoom: 1.0,
            close_requested: false,
            export_csv_requested: false,
            export_image_requested: None,
            clear_requested: false,
        }
    }
}

struct RenderedReconstruction {
    image: ColorImage,
    size: [usize; 2],
}

fn render_reconstruction(
    table: &LocalizationTable,
    settings: ReconstructionRenderSettings,
) -> RenderedReconstruction {
    let width_nm = f64::from(table.sensor_width) * table.nm_per_pixel;
    let height_nm = f64::from(table.sensor_height) * table.nm_per_pixel;
    let width = ((width_nm / settings.pixel_size_nm).ceil() as usize).max(1);
    let height = ((height_nm / settings.pixel_size_nm).ceil() as usize).max(1);

    let mut bins = vec![0u32; width * height];
    for row in &table.rows {
        let x = (row.x_nm / settings.pixel_size_nm).floor() as isize;
        let y = (row.y_nm / settings.pixel_size_nm).floor() as isize;
        if x < 0 || y < 0 {
            continue;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= width || y >= height {
            continue;
        }
        bins[y * width + x] = bins[y * width + x].saturating_add(1);
    }

    let max_value = percentile_nonzero(&bins, settings.contrast_percentile).max(1.0);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for count in bins {
        let normalized = ((count as f32 / max_value).clamp(0.0, 1.0)).sqrt();
        let color = colormap_color(settings.colormap, normalized);
        rgba.extend_from_slice(&color.to_array());
    }

    RenderedReconstruction {
        image: ColorImage::from_rgba_unmultiplied([width, height], &rgba),
        size: [width, height],
    }
}

pub fn export_csv_to_path(path: &Path, table: &LocalizationTable) -> Result<(), String> {
    let file =
        File::create(path).map_err(|err| format!("creating {} failed: {err}", path.display()))?;
    let writer = BufWriter::new(file);
    write_csv(writer, table).map_err(|err| format!("writing {} failed: {err}", path.display()))
}

pub fn export_image_to_path(path: &Path, image: &ColorImage) -> Result<(), String> {
    let width = image.size[0] as u32;
    let height = image.size[1] as u32;
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for pixel in &image.pixels {
        rgba.extend_from_slice(&pixel.to_array());
    }
    let rgba = RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "failed to build RGBA export image".to_owned())?;

    let format = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => ImageFormat::Png,
        Some("tif") | Some("tiff") => ImageFormat::Tiff,
        _ => return Err("image export path must end in .png, .tif, or .tiff".into()),
    };

    rgba.save_with_format(path, format)
        .map_err(|err| format!("saving {} failed: {err}", path.display()))
}

pub fn render_reconstruction_viewport(
    ui: &mut egui::Ui,
    shared: &Arc<Mutex<ReconstructionSharedData>>,
) {
    let (
        texture,
        total_localizations,
        rendered_width,
        rendered_height,
        zoom,
        pixel_size_nm,
        contrast_percentile,
        colormap,
    ) = {
        let mut data = shared.lock().unwrap();

        ui.horizontal_wrapped(|ui| {
            if ui.button("Export CSV").clicked() {
                data.export_csv_requested = true;
            }
            ui.menu_button("Export Image", |ui| {
                if ui.button("PNG").clicked() {
                    data.export_image_requested = Some(ReconstructionImageFormat::Png);
                    ui.close_menu();
                }
                if ui.button("TIFF").clicked() {
                    data.export_image_requested = Some(ReconstructionImageFormat::Tiff);
                    ui.close_menu();
                }
            });
            if ui.button("Clear").clicked() {
                data.clear_requested = true;
            }
            ui.separator();
            ui.label("Pixel size [nm]");
            ui.add(egui::Slider::new(&mut data.pixel_size_nm, 5.0..=100.0));
            ui.label("Contrast");
            ui.add(egui::Slider::new(
                &mut data.contrast_percentile,
                90.0..=100.0,
            ));
            egui::ComboBox::from_id_source("reconstruction_colormap")
                .selected_text(data.colormap.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut data.colormap, ReconstructionColormap::Hot, "Hot");
                    ui.selectable_value(&mut data.colormap, ReconstructionColormap::Gray, "Gray");
                });
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut data.zoom, 0.5..=8.0).logarithmic(true));
        });

        (
            data.texture.clone(),
            data.total_localizations,
            data.rendered_width,
            data.rendered_height,
            data.zoom,
            data.pixel_size_nm,
            data.contrast_percentile,
            data.colormap,
        )
    };

    ui.separator();

    if let Some(texture) = texture {
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let size = texture.size_vec2() * zoom;
                ui.add(egui::Image::new(&texture).fit_to_exact_size(size));
            });
    } else {
        ui.centered_and_justified(|ui| {
            ui.label("No accumulated localizations available yet.");
        });
    }

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Localizations: {total_localizations}"));
        ui.separator();
        ui.label(format!("Image: {rendered_width} x {rendered_height} px"));
        ui.separator();
        ui.label(format!("Pixel size: {:.1} nm", pixel_size_nm));
        ui.separator();
        ui.label(format!("Contrast: {:.1}th percentile", contrast_percentile));
        ui.separator();
        ui.label(format!("Colormap: {}", colormap.label()));
    });
}

fn write_csv(mut writer: impl Write, table: &LocalizationTable) -> std::io::Result<()> {
    writeln!(
        writer,
        "\"id\",\"frame\",\"x [nm]\",\"y [nm]\",\"sigma [nm]\",\"intensity [photon]\",\"offset [photon]\",\"uncertainty_xy [nm]\""
    )?;
    for row in &table.rows {
        writeln!(
            writer,
            "{},{},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
            row.id,
            row.frame,
            row.x_nm,
            row.y_nm,
            row.sigma_nm,
            row.intensity,
            row.offset,
            row.uncertainty_xy_nm,
        )?;
    }
    writer.flush()
}

fn percentile_nonzero(values: &[u32], percentile: f32) -> f32 {
    let mut values: Vec<u32> = values.iter().copied().filter(|value| *value > 0).collect();
    if values.is_empty() {
        return 1.0;
    }
    values.sort_unstable();
    let last = values.len().saturating_sub(1);
    let index = ((last as f32) * percentile.clamp(0.0, 100.0) / 100.0).round() as usize;
    values[index.min(last)] as f32
}

fn colormap_color(colormap: ReconstructionColormap, value: f32) -> Color32 {
    let value = value.clamp(0.0, 1.0);
    match colormap {
        ReconstructionColormap::Gray => {
            let channel = (value * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(channel, channel, channel, 255)
        }
        ReconstructionColormap::Hot => {
            let (r, g, b) = if value < (1.0 / 3.0) {
                (value * 3.0, 0.0, 0.0)
            } else if value < (2.0 / 3.0) {
                (1.0, (value - 1.0 / 3.0) * 3.0, 0.0)
            } else {
                (1.0, 1.0, (value - 2.0 / 3.0) * 3.0)
            };
            Color32::from_rgba_premultiplied(
                (r.clamp(0.0, 1.0) * 255.0).round() as u8,
                (g.clamp(0.0, 1.0) * 255.0).round() as u8,
                (b.clamp(0.0, 1.0) * 255.0).round() as u8,
                255,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use augur_plugin_api::LocalizationRow;

    fn table() -> LocalizationTable {
        LocalizationTable {
            rows: vec![LocalizationRow {
                id: 1,
                frame: 2,
                x_nm: 12.0,
                y_nm: 24.0,
                sigma_nm: 48.0,
                intensity: 100.0,
                offset: 4.0,
                uncertainty_xy_nm: 8.0,
                timestamp_us: 10,
            }],
            nm_per_pixel: 5.0,
            sensor_width: 10,
            sensor_height: 6,
        }
    }

    #[test]
    fn render_dimensions_follow_sensor_size_and_pixel_size() {
        let rendered = render_reconstruction(
            &table(),
            ReconstructionRenderSettings {
                pixel_size_nm: 10.0,
                ..ReconstructionRenderSettings::default()
            },
        );

        assert_eq!(rendered.size, [5, 3]);
    }

    #[test]
    fn csv_export_uses_thunderstorm_header() {
        let mut bytes = Vec::new();
        write_csv(&mut bytes, &table()).expect("csv must write");
        let text = String::from_utf8(bytes).expect("csv must be utf-8");
        let header = text.lines().next().expect("csv must contain header");
        assert_eq!(
            header,
            "\"id\",\"frame\",\"x [nm]\",\"y [nm]\",\"sigma [nm]\",\"intensity [photon]\",\"offset [photon]\",\"uncertainty_xy [nm]\""
        );
    }
}
