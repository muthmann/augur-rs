use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use augur_plugin_api::{
    HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor, HostViewPlacement,
    HostViewRegistry, TableDatasetV1, TableSchema,
};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use image::{ImageFormat, RgbaImage};

pub const COMPACT_TABLE_PREVIEW_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostViewProviderKey {
    Builtin(usize),
    Runtime(usize),
}

#[derive(Debug, Clone)]
pub struct HostRegistryContribution {
    pub provider: HostViewProviderKey,
    pub provider_name: String,
    pub registry: HostViewRegistry,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHostDataset {
    pub descriptor: HostDatasetDescriptor,
    pub provider: HostViewProviderKey,
    pub provider_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHostView {
    pub descriptor: HostViewDescriptor,
    pub provider: HostViewProviderKey,
    pub provider_name: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedHostViewRegistry {
    datasets: Vec<ResolvedHostDataset>,
    views: Vec<ResolvedHostView>,
    dataset_indices: HashMap<String, usize>,
    view_indices: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl ResolvedHostViewRegistry {
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn dataset(&self, id: &str) -> Option<&ResolvedHostDataset> {
        self.dataset_indices
            .get(id)
            .and_then(|index| self.datasets.get(*index))
    }

    pub fn view(&self, id: &str) -> Option<&ResolvedHostView> {
        self.view_indices
            .get(id)
            .and_then(|index| self.views.get(*index))
    }

    pub fn panel_views_for_provider(
        &self,
        provider: HostViewProviderKey,
    ) -> impl Iterator<Item = &ResolvedHostView> {
        self.views.iter().filter(move |view| {
            view.provider == provider
                && view.descriptor.placement == HostViewPlacement::AnalysisPanel
        })
    }

    pub fn window_views(&self) -> impl Iterator<Item = &ResolvedHostView> {
        self.views
            .iter()
            .filter(|view| view.descriptor.placement == HostViewPlacement::Window)
    }
}

pub fn reset_provider_for_dataset(
    dataset: &ResolvedHostDataset,
    mut reset_builtin: impl FnMut(usize),
    mut reset_runtime: impl FnMut(usize),
) {
    match dataset.provider {
        HostViewProviderKey::Builtin(index) => reset_builtin(index),
        HostViewProviderKey::Runtime(index) => reset_runtime(index),
    }
}

pub fn resolve_host_view_registry(
    contributions: impl IntoIterator<Item = HostRegistryContribution>,
) -> ResolvedHostViewRegistry {
    let mut resolved = ResolvedHostViewRegistry::default();

    for contribution in contributions {
        for descriptor in contribution.registry.datasets {
            if descriptor.id.trim().is_empty() {
                resolved.warnings.push(format!(
                    "Ignoring dataset with an empty id from {}.",
                    contribution.provider_name
                ));
                continue;
            }

            if let Some(index) = resolved.dataset_indices.get(&descriptor.id).copied() {
                let current = &mut resolved.datasets[index];
                if current.descriptor == descriptor {
                    current.provider = contribution.provider;
                    current.provider_name = contribution.provider_name.clone();
                } else {
                    resolved.warnings.push(format!(
                        "Ignoring conflicting dataset id {} from {}; it does not match the earlier descriptor from {}.",
                        descriptor.id, contribution.provider_name, current.provider_name
                    ));
                }
                continue;
            }

            resolved
                .dataset_indices
                .insert(descriptor.id.clone(), resolved.datasets.len());
            resolved.datasets.push(ResolvedHostDataset {
                descriptor,
                provider: contribution.provider,
                provider_name: contribution.provider_name.clone(),
            });
        }

        for descriptor in contribution.registry.views {
            if descriptor.id.trim().is_empty() {
                resolved.warnings.push(format!(
                    "Ignoring view with an empty id from {}.",
                    contribution.provider_name
                ));
                continue;
            }

            if let Some(index) = resolved.view_indices.get(&descriptor.id).copied() {
                let current = &mut resolved.views[index];
                if current.descriptor == descriptor {
                    current.provider = contribution.provider;
                    current.provider_name = contribution.provider_name.clone();
                } else {
                    resolved.warnings.push(format!(
                        "Ignoring conflicting view id {} from {}; it does not match the earlier descriptor from {}.",
                        descriptor.id, contribution.provider_name, current.provider_name
                    ));
                }
                continue;
            }

            resolved
                .view_indices
                .insert(descriptor.id.clone(), resolved.views.len());
            resolved.views.push(ResolvedHostView {
                descriptor,
                provider: contribution.provider,
                provider_name: contribution.provider_name.clone(),
            });
        }
    }

    let mut filtered_views = Vec::with_capacity(resolved.views.len());
    let mut filtered_indices = HashMap::new();
    for view in resolved.views.drain(..) {
        if resolved
            .dataset_indices
            .contains_key(&view.descriptor.dataset_id)
        {
            filtered_indices.insert(view.descriptor.id.clone(), filtered_views.len());
            filtered_views.push(view);
        } else {
            resolved.warnings.push(format!(
                "Ignoring view id {} from {} because dataset {} is not resolved.",
                view.descriptor.id, view.provider_name, view.descriptor.dataset_id
            ));
        }
    }
    resolved.views = filtered_views;
    resolved.view_indices = filtered_indices;
    resolved
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostDatasetSnapshot {
    Table(Arc<TableDatasetV1>),
}

pub fn decode_dataset_snapshot(
    descriptor: &HostDatasetDescriptor,
    bytes: &[u8],
) -> Result<HostDatasetSnapshot, String> {
    match &descriptor.kind {
        HostDatasetKind::TableV1(schema) => {
            let dataset: TableDatasetV1 = serde_json::from_slice(bytes)
                .map_err(|err| format!("table dataset JSON is invalid: {err}"))?;
            dataset.validate_against_schema(schema)?;
            Ok(HostDatasetSnapshot::Table(Arc::new(dataset)))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostViewColormap {
    Hot,
    Gray,
}

impl HostViewColormap {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hot => "Hot",
            Self::Gray => "Gray",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostViewImageFormat {
    Png,
    Tiff,
}

impl HostViewImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Tiff => "tiff",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Density2dRenderSettings {
    pub pixel_size: f64,
    pub contrast_percentile: f32,
    pub colormap: HostViewColormap,
    pub zoom: f32,
}

impl Default for Density2dRenderSettings {
    fn default() -> Self {
        Self {
            pixel_size: 10.0,
            contrast_percentile: 99.5,
            colormap: HostViewColormap::Hot,
            zoom: 1.0,
        }
    }
}

#[derive(Default)]
pub struct Density2dViewState {
    image: Option<ColorImage>,
    texture: Option<TextureHandle>,
    rendered_size: [usize; 2],
    dirty: bool,
    settings: Density2dRenderSettings,
}

impl Density2dViewState {
    pub fn clear(&mut self) {
        self.image = None;
        self.texture = None;
        self.rendered_size = [0, 0];
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn settings(&self) -> Density2dRenderSettings {
        self.settings
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

    pub fn set_settings(&mut self, settings: Density2dRenderSettings) {
        if self.settings != settings {
            self.settings = settings;
            self.mark_dirty();
        }
    }

    pub fn render_if_needed(
        &mut self,
        ctx: &egui::Context,
        schema: &TableSchema,
        dataset: Option<&TableDatasetV1>,
        x_column: &str,
        y_column: &str,
    ) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }

        let Some(dataset) = dataset else {
            self.clear();
            return Ok(());
        };

        let rendered = render_density_image(
            schema,
            dataset,
            x_column,
            y_column,
            self.settings.pixel_size,
            self.settings.contrast_percentile,
            self.settings.colormap,
        )?;
        self.rendered_size = rendered.size;
        self.image = Some(rendered.image.clone());
        if let Some(texture) = &mut self.texture {
            texture.set(rendered.image, TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("host_view_density", rendered.image, TextureOptions::LINEAR));
        }
        self.dirty = false;
        Ok(())
    }
}

#[derive(Default)]
pub enum HostViewRenderState {
    #[default]
    None,
    Density2d(Density2dViewState),
}

impl HostViewRenderState {
    pub fn density_state(&mut self) -> &mut Density2dViewState {
        if !matches!(self, Self::Density2d(_)) {
            *self = Self::Density2d(Density2dViewState {
                dirty: true,
                ..Default::default()
            });
        }

        match self {
            Self::Density2d(state) => state,
            Self::None => unreachable!(),
        }
    }

    pub fn mark_dirty(&mut self) {
        if let Self::Density2d(state) = self {
            state.mark_dirty();
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HostViewUiActions {
    pub export_csv: bool,
    pub export_image: Option<HostViewImageFormat>,
    pub clear_requested: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactTableSummary {
    pub shown_rows: usize,
    pub total_rows: usize,
}

impl CompactTableSummary {
    pub fn is_empty(self) -> bool {
        self.total_rows == 0
    }
}

pub fn compact_table_summary(dataset: Option<&TableDatasetV1>) -> CompactTableSummary {
    let total_rows = dataset.map(TableDatasetV1::row_count).unwrap_or(0);
    CompactTableSummary {
        shown_rows: total_rows.min(COMPACT_TABLE_PREVIEW_ROWS),
        total_rows,
    }
}

pub fn render_compact_table(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: Option<&TableDatasetV1>,
    empty_message: &str,
) {
    let summary = compact_table_summary(dataset);
    if summary.is_empty() {
        ui.label(empty_message);
        return;
    }

    render_table_header(ui, schema);
    ui.separator();
    if let Some(dataset) = dataset {
        for row in 0..summary.shown_rows {
            render_table_row(ui, schema, dataset, row);
        }
    }
    ui.separator();
    ui.label(format!(
        "showing {} of {}",
        summary.shown_rows, summary.total_rows
    ));
}

pub fn render_table_window(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: Option<&TableDatasetV1>,
    empty_message: &str,
) -> HostViewUiActions {
    let mut actions = HostViewUiActions::default();
    ui.horizontal(|ui| {
        if ui.button("Export CSV").clicked() {
            actions.export_csv = true;
        }
        ui.separator();
        ui.label(format!(
            "Rows: {}",
            dataset.map(TableDatasetV1::row_count).unwrap_or(0)
        ));
    });
    ui.separator();

    let Some(dataset) = dataset else {
        ui.label(empty_message);
        return actions;
    };

    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    render_table_header(ui, schema);
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, row_height, dataset.row_count(), |ui, range| {
            for row in range {
                render_table_row(ui, schema, dataset, row);
            }
        });
    ui.separator();
    ui.label(format!("{} rows", dataset.row_count()));
    actions
}

pub fn render_density2d_view(
    ui: &mut egui::Ui,
    view_id: &str,
    state: &mut Density2dViewState,
    dataset: Option<&TableDatasetV1>,
    empty_message: &str,
    allow_clear: bool,
) -> HostViewUiActions {
    let mut actions = HostViewUiActions::default();

    ui.horizontal_wrapped(|ui| {
        if ui.button("Export CSV").clicked() {
            actions.export_csv = true;
        }
        ui.menu_button("Export Image", |ui| {
            if ui.button("PNG").clicked() {
                actions.export_image = Some(HostViewImageFormat::Png);
                ui.close_menu();
            }
            if ui.button("TIFF").clicked() {
                actions.export_image = Some(HostViewImageFormat::Tiff);
                ui.close_menu();
            }
        });
        if allow_clear && ui.button("Clear").clicked() {
            actions.clear_requested = true;
        }
        ui.separator();

        let mut settings = state.settings();
        ui.label("Pixel size");
        if ui
            .add(egui::Slider::new(&mut settings.pixel_size, 5.0..=100.0))
            .changed()
        {
            state.settings = settings;
            state.mark_dirty();
        }
        ui.label("Contrast");
        if ui
            .add(egui::Slider::new(
                &mut settings.contrast_percentile,
                90.0..=100.0,
            ))
            .changed()
        {
            state.settings = settings;
            state.mark_dirty();
        }
        egui::ComboBox::from_id_source(format!("{view_id}_colormap"))
            .selected_text(settings.colormap.label())
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(&mut settings.colormap, HostViewColormap::Hot, "Hot")
                    .changed()
                {
                    state.settings = settings;
                    state.mark_dirty();
                }
                if ui
                    .selectable_value(&mut settings.colormap, HostViewColormap::Gray, "Gray")
                    .changed()
                {
                    state.settings = settings;
                    state.mark_dirty();
                }
            });
        ui.label("Zoom");
        if ui
            .add(egui::Slider::new(&mut settings.zoom, 0.5..=8.0).logarithmic(true))
            .changed()
        {
            state.settings = settings;
        }
    });

    ui.separator();

    let Some(dataset) = dataset else {
        ui.label(empty_message);
        return actions;
    };

    if let Some(texture) = &state.texture {
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let size = texture.size_vec2() * state.settings.zoom;
                ui.add(egui::Image::new(texture).fit_to_exact_size(size));
            });
    } else {
        ui.label(empty_message);
    }

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Rows: {}", dataset.row_count()));
        ui.separator();
        ui.label(format!(
            "Image: {} x {} px",
            state.rendered_size[0], state.rendered_size[1]
        ));
        ui.separator();
        ui.label(format!("Pixel size: {:.1}", state.settings.pixel_size));
        ui.separator();
        ui.label(format!(
            "Contrast: {:.1}th percentile",
            state.settings.contrast_percentile
        ));
        ui.separator();
        ui.label(format!("Colormap: {}", state.settings.colormap.label()));
    });

    actions
}

#[derive(Clone, Default)]
pub struct TableWindowViewportData {
    pub schema: TableSchema,
    pub dataset: Option<Arc<TableDatasetV1>>,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
    pub export_csv_requested: bool,
}

#[derive(Clone, Default)]
pub struct DensityWindowViewportData {
    pub texture: Option<TextureHandle>,
    pub total_rows: usize,
    pub rendered_width: usize,
    pub rendered_height: usize,
    pub settings: Density2dRenderSettings,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
    pub export_csv_requested: bool,
    pub export_image_requested: Option<HostViewImageFormat>,
    pub clear_requested: bool,
}

pub fn render_table_window_viewport(
    ui: &mut egui::Ui,
    shared: &Arc<Mutex<TableWindowViewportData>>,
) {
    let (schema, dataset, empty_message, error_message) = {
        let data = shared.lock().unwrap();
        (
            data.schema.clone(),
            data.dataset.clone(),
            data.empty_message.clone(),
            data.error_message.clone(),
        )
    };

    if let Some(error) = error_message {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return;
    }

    let actions = render_table_window(ui, &schema, dataset.as_deref(), &empty_message);
    if actions.export_csv {
        let mut data = shared.lock().unwrap();
        data.export_csv_requested = true;
    }
}

pub fn render_density_window_viewport(
    ui: &mut egui::Ui,
    view_id: &str,
    shared: &Arc<Mutex<DensityWindowViewportData>>,
) {
    let (
        texture,
        total_rows,
        rendered_width,
        rendered_height,
        settings,
        empty_message,
        error_message,
    ) = {
        let mut data = shared.lock().unwrap();

        ui.horizontal_wrapped(|ui| {
            if ui.button("Export CSV").clicked() {
                data.export_csv_requested = true;
            }
            ui.menu_button("Export Image", |ui| {
                if ui.button("PNG").clicked() {
                    data.export_image_requested = Some(HostViewImageFormat::Png);
                    ui.close_menu();
                }
                if ui.button("TIFF").clicked() {
                    data.export_image_requested = Some(HostViewImageFormat::Tiff);
                    ui.close_menu();
                }
            });
            if ui.button("Clear").clicked() {
                data.clear_requested = true;
            }
            ui.separator();
            ui.label("Pixel size");
            ui.add(egui::Slider::new(
                &mut data.settings.pixel_size,
                5.0..=100.0,
            ));
            ui.label("Contrast");
            ui.add(egui::Slider::new(
                &mut data.settings.contrast_percentile,
                90.0..=100.0,
            ));
            egui::ComboBox::from_id_source(format!("{view_id}_viewport_colormap"))
                .selected_text(data.settings.colormap.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut data.settings.colormap, HostViewColormap::Hot, "Hot");
                    ui.selectable_value(
                        &mut data.settings.colormap,
                        HostViewColormap::Gray,
                        "Gray",
                    );
                });
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut data.settings.zoom, 0.5..=8.0).logarithmic(true));
        });

        (
            data.texture.clone(),
            data.total_rows,
            data.rendered_width,
            data.rendered_height,
            data.settings,
            data.empty_message.clone(),
            data.error_message.clone(),
        )
    };

    ui.separator();

    if let Some(error) = error_message {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return;
    }

    if let Some(texture) = texture {
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let size = texture.size_vec2() * settings.zoom;
                ui.add(egui::Image::new(&texture).fit_to_exact_size(size));
            });
    } else {
        ui.centered_and_justified(|ui| {
            ui.label(empty_message);
        });
    }

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Rows: {total_rows}"));
        ui.separator();
        ui.label(format!("Image: {rendered_width} x {rendered_height} px"));
        ui.separator();
        ui.label(format!("Pixel size: {:.1}", settings.pixel_size));
        ui.separator();
        ui.label(format!(
            "Contrast: {:.1}th percentile",
            settings.contrast_percentile
        ));
        ui.separator();
        ui.label(format!("Colormap: {}", settings.colormap.label()));
    });
}

pub fn export_table_csv_to_path(
    path: &Path,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
) -> Result<(), String> {
    let file =
        File::create(path).map_err(|err| format!("creating {} failed: {err}", path.display()))?;
    let writer = BufWriter::new(file);
    write_table_csv(writer, schema, dataset)
        .map_err(|err| format!("writing {} failed: {err}", path.display()))
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

fn render_table_header(ui: &mut egui::Ui, schema: &TableSchema) {
    ui.horizontal_wrapped(|ui| {
        for column in &schema.columns {
            ui.monospace(&column.title);
            ui.add_space(8.0);
        }
    });
}

fn render_table_row(ui: &mut egui::Ui, schema: &TableSchema, dataset: &TableDatasetV1, row: usize) {
    ui.horizontal_wrapped(|ui| {
        for column in &schema.columns {
            let value = dataset
                .column(&column.id)
                .and_then(|column| column.values.display_value(row))
                .unwrap_or_else(|| "-".into());
            ui.label(value);
            ui.add_space(8.0);
        }
    });
}

fn write_table_csv(
    mut writer: impl Write,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
) -> std::io::Result<()> {
    for (index, column) in schema.columns.iter().enumerate() {
        if index > 0 {
            write!(writer, ",")?;
        }
        write!(writer, "\"{}\"", column.title.replace('"', "\"\""))?;
    }
    writeln!(writer)?;

    for row in 0..dataset.row_count() {
        for (index, column) in schema.columns.iter().enumerate() {
            if index > 0 {
                write!(writer, ",")?;
            }
            let value = dataset
                .column(&column.id)
                .and_then(|column| column.values.display_value(row))
                .unwrap_or_default();
            write!(writer, "\"{}\"", value.replace('"', "\"\""))?;
        }
        writeln!(writer)?;
    }
    writer.flush()
}

struct RenderedDensityImage {
    image: ColorImage,
    size: [usize; 2],
}

fn render_density_image(
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    x_column: &str,
    y_column: &str,
    pixel_size: f64,
    contrast_percentile: f32,
    colormap: HostViewColormap,
) -> Result<RenderedDensityImage, String> {
    let x_values = dataset
        .column(x_column)
        .ok_or_else(|| format!("dataset is missing x column {x_column}"))?;
    let y_values = dataset
        .column(y_column)
        .ok_or_else(|| format!("dataset is missing y column {y_column}"))?;

    let coordinate_space = schema
        .coordinate_space_2d
        .as_ref()
        .filter(|space| space.x_column == x_column && space.y_column == y_column);

    let (x_min, x_max, y_min, y_max) = if let Some(space) = coordinate_space {
        (space.x_min, space.x_max, space.y_min, space.y_max)
    } else {
        let xs = collect_numeric_column(x_values, x_column)?;
        let ys = collect_numeric_column(y_values, y_column)?;
        if xs.is_empty() || ys.is_empty() {
            (0.0, pixel_size, 0.0, pixel_size)
        } else {
            (
                xs.iter().copied().fold(f64::INFINITY, f64::min),
                xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                ys.iter().copied().fold(f64::INFINITY, f64::min),
                ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            )
        }
    };

    let width = (((x_max - x_min).max(0.0) / pixel_size).ceil() as usize).max(1);
    let height = (((y_max - y_min).max(0.0) / pixel_size).ceil() as usize).max(1);
    let mut bins = vec![0u32; width * height];

    for row in 0..dataset.row_count() {
        let Some(x) = x_values.values.numeric_value(row) else {
            return Err(format!("column {x_column} is not numeric"));
        };
        let Some(y) = y_values.values.numeric_value(row) else {
            return Err(format!("column {y_column} is not numeric"));
        };
        let x = ((x - x_min) / pixel_size).floor() as isize;
        let y = ((y - y_min) / pixel_size).floor() as isize;
        if x < 0 || y < 0 {
            continue;
        }
        let (x, y) = (x as usize, y as usize);
        if x >= width || y >= height {
            continue;
        }
        bins[y * width + x] = bins[y * width + x].saturating_add(1);
    }

    let max_value = percentile_nonzero(&bins, contrast_percentile).max(1.0);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for count in bins {
        let normalized = ((count as f32 / max_value).clamp(0.0, 1.0)).sqrt();
        rgba.extend_from_slice(&colormap_color(colormap, normalized).to_array());
    }

    Ok(RenderedDensityImage {
        image: ColorImage::from_rgba_unmultiplied([width, height], &rgba),
        size: [width, height],
    })
}

fn collect_numeric_column(
    column: &augur_plugin_api::TableColumnData,
    column_id: &str,
) -> Result<Vec<f64>, String> {
    let mut values = Vec::with_capacity(column.len());
    for index in 0..column.len() {
        let Some(value) = column.values.numeric_value(index) else {
            return Err(format!("column {column_id} is not numeric"));
        };
        values.push(value);
    }
    Ok(values)
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

fn colormap_color(colormap: HostViewColormap, value: f32) -> Color32 {
    let value = value.clamp(0.0, 1.0);
    match colormap {
        HostViewColormap::Gray => {
            let channel = (value * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(channel, channel, channel, 255)
        }
        HostViewColormap::Hot => {
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
    use augur_plugin_api::{
        HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor, HostViewKind,
        HostViewPlacement, TableColumn, TableColumnData, TableColumnValues, TableDatasetV1,
        TableSchema, TableValueType,
    };

    fn table_schema() -> TableSchema {
        TableSchema {
            columns: vec![
                TableColumn {
                    id: "frame".into(),
                    title: "Frame".into(),
                    value_type: TableValueType::U64,
                },
                TableColumn {
                    id: "x_nm".into(),
                    title: "X [nm]".into(),
                    value_type: TableValueType::F64,
                },
            ],
            coordinate_space_2d: None,
        }
    }

    fn table_dataset() -> TableDatasetV1 {
        TableDatasetV1::new(vec![
            TableColumnData {
                column_id: "frame".into(),
                values: TableColumnValues::U64(vec![1, 2, 3]),
            },
            TableColumnData {
                column_id: "x_nm".into(),
                values: TableColumnValues::F64(vec![10.0, 11.0, 12.0]),
            },
        ])
        .expect("dataset must validate")
    }

    fn contribution(
        provider: HostViewProviderKey,
        provider_name: &str,
        dataset: HostDatasetDescriptor,
        view: HostViewDescriptor,
    ) -> HostRegistryContribution {
        HostRegistryContribution {
            provider,
            provider_name: provider_name.into(),
            registry: HostViewRegistry {
                datasets: vec![dataset],
                views: vec![view],
            },
        }
    }

    #[test]
    fn conflicting_duplicate_ids_are_ignored() {
        let first = contribution(
            HostViewProviderKey::Runtime(0),
            "First",
            HostDatasetDescriptor {
                id: "dataset.localization".into(),
                title: "Localizations".into(),
                kind: HostDatasetKind::TableV1(table_schema()),
                empty_message: "No rows".into(),
            },
            HostViewDescriptor {
                id: "view.localization".into(),
                title: "Preview".into(),
                dataset_id: "dataset.localization".into(),
                placement: HostViewPlacement::AnalysisPanel,
                kind: HostViewKind::CompactTable,
            },
        );
        let second = contribution(
            HostViewProviderKey::Runtime(1),
            "Second",
            HostDatasetDescriptor {
                id: "dataset.localization".into(),
                title: "Different".into(),
                kind: HostDatasetKind::TableV1(table_schema()),
                empty_message: "No rows".into(),
            },
            HostViewDescriptor {
                id: "view.localization".into(),
                title: "Preview".into(),
                dataset_id: "dataset.localization".into(),
                placement: HostViewPlacement::AnalysisPanel,
                kind: HostViewKind::TableWindow,
            },
        );

        let resolved = resolve_host_view_registry([first, second]);
        assert_eq!(resolved.datasets.len(), 1);
        assert_eq!(resolved.views.len(), 1);
        assert_eq!(
            resolved
                .dataset("dataset.localization")
                .unwrap()
                .provider_name,
            "First"
        );
        assert_eq!(
            resolved.view("view.localization").unwrap().provider_name,
            "First"
        );
        assert_eq!(resolved.warnings().len(), 2);
    }

    #[test]
    fn later_matching_descriptors_override_provider_ownership() {
        let dataset = HostDatasetDescriptor {
            id: "dataset.localization".into(),
            title: "Localizations".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
        };
        let view = HostViewDescriptor {
            id: "view.localization".into(),
            title: "Preview".into(),
            dataset_id: "dataset.localization".into(),
            placement: HostViewPlacement::Window,
            kind: HostViewKind::TableWindow,
        };

        let resolved = resolve_host_view_registry([
            contribution(
                HostViewProviderKey::Runtime(0),
                "First",
                dataset.clone(),
                view.clone(),
            ),
            contribution(HostViewProviderKey::Runtime(1), "Second", dataset, view),
        ]);

        assert_eq!(
            resolved
                .dataset("dataset.localization")
                .unwrap()
                .provider_name,
            "Second"
        );
        assert_eq!(
            resolved.view("view.localization").unwrap().provider_name,
            "Second"
        );
        assert!(resolved.warnings().is_empty());
    }

    #[test]
    fn compact_table_summary_reports_empty_and_populated_states() {
        let empty = compact_table_summary(None);
        assert!(empty.is_empty());
        assert_eq!(empty.shown_rows, 0);

        let populated = compact_table_summary(Some(&table_dataset()));
        assert!(!populated.is_empty());
        assert_eq!(populated.shown_rows, 3);
        assert_eq!(populated.total_rows, 3);
    }

    #[test]
    fn decode_dataset_snapshot_validates_table_schema() {
        let descriptor = HostDatasetDescriptor {
            id: "dataset.localization".into(),
            title: "Localizations".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
        };
        let bytes = serde_json::to_vec(&table_dataset()).expect("dataset json");

        let snapshot = decode_dataset_snapshot(&descriptor, &bytes).expect("snapshot");
        assert_eq!(
            snapshot,
            HostDatasetSnapshot::Table(Arc::new(table_dataset()))
        );
    }

    #[test]
    fn window_views_appear_only_for_window_placements() {
        let dataset = HostDatasetDescriptor {
            id: "dataset.localization".into(),
            title: "Localizations".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
        };
        let resolved = resolve_host_view_registry([
            contribution(
                HostViewProviderKey::Runtime(0),
                "Provider",
                dataset.clone(),
                HostViewDescriptor {
                    id: "view.panel".into(),
                    title: "Panel".into(),
                    dataset_id: "dataset.localization".into(),
                    placement: HostViewPlacement::AnalysisPanel,
                    kind: HostViewKind::CompactTable,
                },
            ),
            contribution(
                HostViewProviderKey::Runtime(0),
                "Provider",
                dataset,
                HostViewDescriptor {
                    id: "view.window".into(),
                    title: "Window".into(),
                    dataset_id: "dataset.localization".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::TableWindow,
                },
            ),
        ]);

        let window_titles: Vec<String> = resolved
            .window_views()
            .map(|view| view.descriptor.title.clone())
            .collect();
        assert_eq!(window_titles, vec!["Window".to_owned()]);

        let empty = resolve_host_view_registry(std::iter::empty::<HostRegistryContribution>());
        assert_eq!(empty.window_views().count(), 0);
    }

    #[test]
    fn reset_provider_for_dataset_targets_only_the_resolved_owner() {
        let dataset = ResolvedHostDataset {
            descriptor: HostDatasetDescriptor {
                id: "dataset.localization".into(),
                title: "Localizations".into(),
                kind: HostDatasetKind::TableV1(table_schema()),
                empty_message: "No rows".into(),
            },
            provider: HostViewProviderKey::Runtime(2),
            provider_name: "Runtime".into(),
        };

        let mut builtin_calls = Vec::new();
        let mut runtime_calls = Vec::new();
        reset_provider_for_dataset(
            &dataset,
            |index| builtin_calls.push(index),
            |index| runtime_calls.push(index),
        );

        assert!(builtin_calls.is_empty());
        assert_eq!(runtime_calls, vec![2]);
    }
}
