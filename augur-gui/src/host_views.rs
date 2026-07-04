use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use augur_plugin_api::{
    HostActionDescriptor, HostActionScope, HostDatasetDescriptor, HostDatasetKind,
    HostViewDescriptor, HostViewKind, HostViewPlacement, HostViewRegistry, Image2dV1, Series1dV1,
    TableColumnDisplayFormat, TableColumnDisplayMetadata, TableColumnValues, TableDatasetV1,
    TableSchema,
};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use egui_plot::{Legend, Line, Plot, PlotPoints, Points};
use image::{ImageFormat, RgbaImage};

use crate::{
    colormap::Colormap,
    investigation::{
        row_index_for_key, row_key_for_row, InvestigationSortDirection,
        InvestigationTableViewState, StableRowKey, TablePageSize,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostViewProviderKey {
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

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHostAction {
    pub descriptor: HostActionDescriptor,
    pub provider: HostViewProviderKey,
    pub provider_name: String,
}

impl ResolvedHostAction {
    pub fn dataset_id(&self) -> &str {
        match &self.descriptor.scope {
            HostActionScope::Dataset { dataset_id }
            | HostActionScope::Row { dataset_id }
            | HostActionScope::Cluster { dataset_id, .. } => dataset_id.as_str(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedHostViewRegistry {
    datasets: Vec<ResolvedHostDataset>,
    views: Vec<ResolvedHostView>,
    actions: Vec<ResolvedHostAction>,
    dataset_indices: HashMap<String, usize>,
    view_indices: HashMap<String, usize>,
    action_indices: HashMap<String, usize>,
    warnings: Vec<String>,
}

impl ResolvedHostViewRegistry {
    pub fn datasets(&self) -> impl Iterator<Item = &ResolvedHostDataset> {
        self.datasets.iter()
    }

    pub fn views(&self) -> impl Iterator<Item = &ResolvedHostView> {
        self.views.iter()
    }

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

    pub fn window_views_for_provider(
        &self,
        provider: HostViewProviderKey,
    ) -> impl Iterator<Item = &ResolvedHostView> {
        self.views.iter().filter(move |view| {
            view.provider == provider && view.descriptor.placement == HostViewPlacement::Window
        })
    }

    pub fn actions(&self) -> impl Iterator<Item = &ResolvedHostAction> {
        self.actions.iter()
    }
}

fn host_view_kind_matches_dataset(
    view_kind: &HostViewKind,
    dataset_kind: &HostDatasetKind,
) -> bool {
    matches!(
        (view_kind, dataset_kind),
        (
            HostViewKind::CompactTable
                | HostViewKind::TableWindow
                | HostViewKind::Density2dFromTable { .. }
                | HostViewKind::Scatter2dFromTable { .. }
                | HostViewKind::Scatter3dFromTable { .. },
            HostDatasetKind::TableV1(_)
        ) | (HostViewKind::ImageWindow, HostDatasetKind::Image2dV1)
            | (HostViewKind::LineSeriesWindow, HostDatasetKind::Series1dV1)
    )
}

fn host_view_kind_label(kind: &HostViewKind) -> &'static str {
    match kind {
        HostViewKind::CompactTable => "compact table",
        HostViewKind::TableWindow => "table",
        HostViewKind::Density2dFromTable { .. } => "density",
        HostViewKind::Scatter2dFromTable { .. } => "scatter",
        HostViewKind::Scatter3dFromTable { .. } => "scatter-3d",
        HostViewKind::ImageWindow => "image",
        HostViewKind::LineSeriesWindow => "line-series",
    }
}

pub fn reset_provider_for_dataset(
    dataset: &ResolvedHostDataset,
    mut reset_runtime: impl FnMut(usize),
) {
    match dataset.provider {
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

        for descriptor in contribution.registry.actions {
            if descriptor.id.trim().is_empty() {
                resolved.warnings.push(format!(
                    "Ignoring action with an empty id from {}.",
                    contribution.provider_name
                ));
                continue;
            }

            if let Some(index) = resolved.action_indices.get(&descriptor.id).copied() {
                let current = &mut resolved.actions[index];
                if current.descriptor == descriptor {
                    current.provider = contribution.provider;
                    current.provider_name = contribution.provider_name.clone();
                } else {
                    resolved.warnings.push(format!(
                        "Ignoring conflicting action id {} from {}; it does not match the earlier descriptor from {}.",
                        descriptor.id, contribution.provider_name, current.provider_name
                    ));
                }
                continue;
            }

            resolved
                .action_indices
                .insert(descriptor.id.clone(), resolved.actions.len());
            resolved.actions.push(ResolvedHostAction {
                descriptor,
                provider: contribution.provider,
                provider_name: contribution.provider_name.clone(),
            });
        }
    }

    let mut filtered_views = Vec::with_capacity(resolved.views.len());
    let mut filtered_indices = HashMap::new();
    for view in resolved.views.drain(..) {
        let Some(dataset_index) = resolved
            .dataset_indices
            .get(&view.descriptor.dataset_id)
            .copied()
        else {
            resolved.warnings.push(format!(
                "Ignoring view id {} from {} because dataset {} is not resolved.",
                view.descriptor.id, view.provider_name, view.descriptor.dataset_id
            ));
            continue;
        };

        let dataset = &resolved.datasets[dataset_index];
        if host_view_kind_matches_dataset(&view.descriptor.kind, &dataset.descriptor.kind) {
            filtered_indices.insert(view.descriptor.id.clone(), filtered_views.len());
            filtered_views.push(view);
        } else {
            resolved.warnings.push(format!(
                "Ignoring view id {} from {} because {} views do not match dataset {} ({:?}).",
                view.descriptor.id,
                view.provider_name,
                host_view_kind_label(&view.descriptor.kind),
                view.descriptor.dataset_id,
                dataset.descriptor.kind
            ));
        }
    }
    resolved.views = filtered_views;
    resolved.view_indices = filtered_indices;

    let mut filtered_actions = Vec::with_capacity(resolved.actions.len());
    let mut filtered_action_indices = HashMap::new();
    for action in resolved.actions.drain(..) {
        let dataset_id = action.dataset_id().to_string();
        if !resolved.dataset_indices.contains_key(&dataset_id) {
            resolved.warnings.push(format!(
                "Ignoring action id {} from {} because dataset {} is not resolved.",
                action.descriptor.id, action.provider_name, dataset_id
            ));
            continue;
        }
        filtered_action_indices.insert(action.descriptor.id.clone(), filtered_actions.len());
        filtered_actions.push(action);
    }
    resolved.actions = filtered_actions;
    resolved.action_indices = filtered_action_indices;

    resolved
}

pub use augur_runtime::{decode_dataset_snapshot, HostDatasetSnapshot};

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
    pub colormap: Colormap,
    pub zoom: f32,
}

impl Default for Density2dRenderSettings {
    fn default() -> Self {
        Self {
            pixel_size: 10.0,
            contrast_percentile: 99.5,
            colormap: Colormap::Fire,
            zoom: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Image2dRenderSettings {
    pub contrast_percentile: f32,
    pub colormap: Colormap,
    pub zoom: f32,
}

impl Default for Image2dRenderSettings {
    fn default() -> Self {
        Self {
            contrast_percentile: 99.5,
            colormap: Colormap::Fire,
            zoom: 1.0,
        }
    }
}

#[derive(Default)]
pub struct Density2dViewState {
    image: Option<ColorImage>,
    texture: Option<TextureHandle>,
    rendered_size: [usize; 2],
    rendered_generation: Option<u64>,
    dirty: bool,
    settings: Density2dRenderSettings,
}

impl Density2dViewState {
    pub fn clear(&mut self) {
        self.image = None;
        self.texture = None;
        self.rendered_size = [0, 0];
        self.rendered_generation = None;
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
        dataset_generation: u64,
        x_column: &str,
        y_column: &str,
    ) -> Result<(), String> {
        if !self.dirty && self.rendered_generation == Some(dataset_generation) {
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
        if let Some(texture) = &mut self.texture {
            texture.set(rendered.image.clone(), TextureOptions::LINEAR);
        } else {
            self.texture = Some(ctx.load_texture(
                "host_view_density",
                rendered.image.clone(),
                TextureOptions::LINEAR,
            ));
        }
        self.image = Some(rendered.image);
        self.rendered_generation = Some(dataset_generation);
        self.dirty = false;
        Ok(())
    }
}

#[derive(Default)]
pub struct Image2dViewState {
    image: Option<ColorImage>,
    texture: Option<TextureHandle>,
    rendered_size: [usize; 2],
    rendered_generation: Option<u64>,
    dirty: bool,
    settings: Image2dRenderSettings,
}

impl Image2dViewState {
    pub fn clear(&mut self) {
        self.image = None;
        self.texture = None;
        self.rendered_size = [0, 0];
        self.rendered_generation = None;
        self.dirty = false;
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn settings(&self) -> Image2dRenderSettings {
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

    pub fn set_settings(&mut self, settings: Image2dRenderSettings) {
        if self.settings != settings {
            self.settings = settings;
            self.mark_dirty();
        }
    }

    pub fn render_if_needed(
        &mut self,
        ctx: &egui::Context,
        dataset: Option<&Image2dV1>,
        dataset_generation: u64,
    ) -> Result<(), String> {
        if !self.dirty && self.rendered_generation == Some(dataset_generation) {
            return Ok(());
        }

        let Some(dataset) = dataset else {
            self.clear();
            return Ok(());
        };

        let image = render_image2d_dataset(
            dataset,
            self.settings.contrast_percentile,
            self.settings.colormap,
        )?;
        self.rendered_size = image.size;
        if let Some(texture) = &mut self.texture {
            texture.set(image.clone(), TextureOptions::LINEAR);
        } else {
            self.texture =
                Some(ctx.load_texture("host_view_image", image.clone(), TextureOptions::LINEAR));
        }
        self.image = Some(image);
        self.rendered_generation = Some(dataset_generation);
        self.dirty = false;
        Ok(())
    }
}

#[derive(Default)]
pub enum HostViewRenderState {
    #[default]
    None,
    Density2d(Density2dViewState),
    Image2d(Image2dViewState),
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
            Self::Image2d(_) => unreachable!(),
        }
    }

    pub fn image_state(&mut self) -> &mut Image2dViewState {
        if !matches!(self, Self::Image2d(_)) {
            *self = Self::Image2d(Image2dViewState {
                dirty: true,
                ..Default::default()
            });
        }

        match self {
            Self::Image2d(state) => state,
            Self::None | Self::Density2d(_) => unreachable!(),
        }
    }

    pub fn mark_dirty(&mut self) {
        match self {
            Self::Density2d(state) => state.mark_dirty(),
            Self::Image2d(state) => state.mark_dirty(),
            Self::None => {}
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct HostViewUiActions {
    pub export_csv: bool,
    pub export_image: Option<HostViewImageFormat>,
    pub clear_requested: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Scatter2dViewOptions<'a> {
    pub view_id: &'a str,
    pub x_column: &'a str,
    pub y_column: &'a str,
    pub allow_clear: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TableCellFormatOptions {
    /// Replay-origin timestamp (µs) used to render relative `mm:ss.uuu`.
    /// When `None`, `TimestampMicros` falls back to absolute µs.
    pub replay_origin_us: Option<u64>,
}

/// Render a cell value according to its display metadata. Raw values are used
/// when no metadata is present; formatting never leaks into CSV exports.
pub fn format_cell_value(
    values: &TableColumnValues,
    index: usize,
    display: Option<&TableColumnDisplayMetadata>,
    options: TableCellFormatOptions,
) -> String {
    let raw = values.display_value(index).unwrap_or_else(|| "-".into());
    let Some(meta) = display else { return raw };
    let Some(format) = meta.format.as_ref() else {
        return raw;
    };
    match format {
        TableColumnDisplayFormat::TimestampMicros => {
            format_timestamp_micros(values_as_u64(values, index), options.replay_origin_us)
                .unwrap_or(raw)
        }
        TableColumnDisplayFormat::FixedPrecision { digits } => match values {
            TableColumnValues::F64(v) => v
                .get(index)
                .map(|value| format!("{:.*}", *digits as usize, value))
                .unwrap_or(raw),
            _ => raw,
        },
        TableColumnDisplayFormat::Identifier | TableColumnDisplayFormat::Category => raw,
    }
}

fn values_as_u64(values: &TableColumnValues, index: usize) -> Option<u64> {
    match values {
        TableColumnValues::U64(v) => v.get(index).copied(),
        TableColumnValues::I64(v) => v.get(index).copied().map(|value| value as u64),
        _ => None,
    }
}

fn format_timestamp_micros(value_us: Option<u64>, replay_origin_us: Option<u64>) -> Option<String> {
    let value = value_us?;
    let relative = match replay_origin_us {
        Some(origin) => value.saturating_sub(origin),
        None => return Some(format!("{value} µs")),
    };
    let total_millis = relative / 1_000;
    let seconds = total_millis / 1_000;
    let minutes = seconds / 60;
    let secs = seconds % 60;
    let millis = (total_millis % 1_000) as u32;
    Some(format!("{minutes:02}:{secs:02}.{millis:03}"))
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SummaryCardOptions<'a> {
    pub dataset_id: &'a str,
    pub generation: u64,
    pub selected_row: Option<&'a StableRowKey>,
    pub format: TableCellFormatOptions,
    pub allow_export: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SummaryCardOutput {
    pub actions: HostViewUiActions,
    pub open_full_table: bool,
}

/// Render a summary card: row count, selected-row detail (formatted via
/// `TableColumnDisplayMetadata`, respecting `hide_in_compact` and `headline`),
/// and action buttons. Replaces the old 10-row free-text preview.
pub fn render_summary_card(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: Option<&TableDatasetV1>,
    empty_message: &str,
    options: SummaryCardOptions<'_>,
) -> SummaryCardOutput {
    let mut output = SummaryCardOutput::default();
    let total_rows = dataset.map(TableDatasetV1::row_count).unwrap_or(0);

    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Rows: {total_rows}"));
        ui.separator();
        if ui.button("Open full table").clicked() {
            output.open_full_table = true;
        }
        if options.allow_export && total_rows > 0 && ui.button("Export CSV").clicked() {
            output.actions.export_csv = true;
        }
    });

    if total_rows == 0 {
        ui.separator();
        ui.label(empty_message);
        return output;
    }

    let Some(dataset) = dataset else {
        return output;
    };

    let selected_row_index = options.selected_row.and_then(|key| {
        row_index_for_key(options.dataset_id, dataset, schema, options.generation, key)
    });

    ui.separator();
    match selected_row_index {
        Some(row) => render_summary_detail(
            ui,
            schema,
            dataset,
            row,
            options.dataset_id,
            options.generation,
            options.format,
        ),
        None => {
            ui.small("Select a row in the full table to see details here.");
        }
    }

    output
}

fn render_summary_detail(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    row: usize,
    dataset_id: &str,
    generation: u64,
    format_options: TableCellFormatOptions,
) {
    let display_map: std::collections::HashMap<&str, &TableColumnDisplayMetadata> = schema
        .column_display
        .iter()
        .map(|e| (e.column_id.as_str(), &e.display))
        .collect();
    let is_headline = |column_id: &str| {
        display_map
            .get(column_id)
            .map(|d| d.headline)
            .unwrap_or(false)
    };

    if let Some(column) = schema.columns.iter().find(|c| is_headline(&c.id)) {
        if let Some(values) = dataset.column(&column.id) {
            let display = display_map.get(column.id.as_str()).copied();
            let formatted = format_cell_value(&values.values, row, display, format_options);
            ui.heading(formatted);
        }
    }

    egui::Grid::new(("summary_card_grid", dataset_id, generation, row))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            for column in &schema.columns {
                let display = display_map.get(column.id.as_str()).copied();
                if display.map(|d| d.hide_in_compact).unwrap_or(false) {
                    continue;
                }
                if is_headline(&column.id) {
                    continue;
                }
                let Some(column_data) = dataset.column(&column.id) else {
                    continue;
                };
                let label = display
                    .and_then(|d| d.label.as_deref())
                    .unwrap_or(&column.title);
                ui.label(label);
                let value = format_cell_value(&column_data.values, row, display, format_options);
                ui.monospace(value);
                ui.end_row();
            }
        });
}

#[derive(Debug, Clone, Copy)]
pub struct LinkedTableViewOptions<'a> {
    pub dataset_id: &'a str,
    pub generation: u64,
    pub rows: &'a [usize],
    pub selected_row: Option<&'a StableRowKey>,
    pub hovered_row: Option<&'a StableRowKey>,
    pub allow_export: bool,
    pub allow_clear: bool,
    pub format: TableCellFormatOptions,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LinkedTableViewOutput {
    pub actions: HostViewUiActions,
    pub selected_row: Option<StableRowKey>,
    pub sort_column: Option<String>,
    pub page_size: Option<TablePageSize>,
    pub page_index: Option<usize>,
}

pub fn render_linked_table_view(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: Option<&TableDatasetV1>,
    table_state: &InvestigationTableViewState,
    empty_message: &str,
    options: LinkedTableViewOptions<'_>,
) -> LinkedTableViewOutput {
    use egui_extras::{Column, TableBuilder};

    let mut output = LinkedTableViewOutput::default();
    let total_rows_in_dataset = dataset.map(TableDatasetV1::row_count).unwrap_or(0);
    let filtered_total = options.rows.len();
    let mut effective_state = table_state.clone();
    effective_state.clamp_page(filtered_total);
    let (page_start, page_end) = effective_state.visible_slice(filtered_total);

    ui.horizontal_wrapped(|ui| {
        if options.allow_export && ui.button("Export CSV").clicked() {
            output.actions.export_csv = true;
        }
        if options.allow_clear && ui.button("Clear").clicked() {
            output.actions.clear_requested = true;
        }
        ui.separator();
        ui.label(format!("Rows: {filtered_total} / {total_rows_in_dataset}",));
        ui.separator();
        ui.label("Page size");
        let mut selected_size = effective_state.page_size;
        egui::ComboBox::from_id_source(("table_page_size", options.dataset_id))
            .selected_text(selected_size.label())
            .show_ui(ui, |ui| {
                for size in TablePageSize::ALL {
                    if ui
                        .selectable_value(&mut selected_size, size, size.label())
                        .changed()
                    {}
                }
            });
        if selected_size != effective_state.page_size {
            output.page_size = Some(selected_size);
            output.page_index = Some(0);
        }
        let page_count = effective_state.page_count(filtered_total);
        ui.separator();
        ui.label(format!(
            "Page {}/{}",
            effective_state.page_index + 1,
            page_count
        ));
        if ui
            .add_enabled(effective_state.page_index > 0, egui::Button::new("◀"))
            .clicked()
        {
            output.page_index = Some(effective_state.page_index - 1);
        }
        if ui
            .add_enabled(
                effective_state.page_index + 1 < page_count,
                egui::Button::new("▶"),
            )
            .clicked()
        {
            output.page_index = Some(effective_state.page_index + 1);
        }
    });
    ui.separator();

    let Some(dataset) = dataset else {
        ui.label(empty_message);
        return output;
    };

    if options.rows.is_empty() {
        ui.small("No rows match the current linked ROI/filter state.");
        return output;
    }

    let page_rows: Vec<usize> = options.rows[page_start..page_end].to_vec();
    let page_keys: Vec<StableRowKey> = page_rows
        .iter()
        .map(|&row| row_key_for_row(options.dataset_id, options.generation, schema, dataset, row))
        .collect();
    let display_map: std::collections::HashMap<&str, &TableColumnDisplayMetadata> = schema
        .column_display
        .iter()
        .map(|e| (e.column_id.as_str(), &e.display))
        .collect();
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;

    let mut builder = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    for column in &schema.columns {
        let width_priority = display_map
            .get(column.id.as_str())
            .and_then(|d| d.width_priority);
        let initial = match width_priority {
            Some(augur_plugin_api::TableColumnWidthPriority::High) => 160.0,
            Some(augur_plugin_api::TableColumnWidthPriority::Medium) => 100.0,
            Some(augur_plugin_api::TableColumnWidthPriority::Low) => 60.0,
            None => 100.0,
        };
        builder = builder.column(Column::initial(initial).at_least(40.0).resizable(true));
    }

    builder
        .header(row_height, |mut header| {
            for column in &schema.columns {
                header.col(|ui| {
                    let sort_indicator =
                        if effective_state.sort_column.as_deref() == Some(&column.id) {
                            match effective_state.sort_direction {
                                InvestigationSortDirection::Ascending => " ↑",
                                InvestigationSortDirection::Descending => " ↓",
                            }
                        } else {
                            ""
                        };
                    let label = display_map
                        .get(column.id.as_str())
                        .and_then(|d| d.label.as_deref())
                        .unwrap_or(&column.title);
                    if ui
                        .small_button(format!("{label}{sort_indicator}"))
                        .clicked()
                    {
                        output.sort_column = Some(column.id.clone());
                    }
                });
            }
        })
        .body(|body| {
            body.rows(row_height, page_rows.len(), |mut row_ui| {
                let row_index_in_page = row_ui.index();
                let Some(row) = page_rows.get(row_index_in_page).copied() else {
                    return;
                };
                let key = &page_keys[row_index_in_page];
                let is_selected = options.selected_row == Some(key);
                let is_hovered = options.hovered_row == Some(key);
                for column in &schema.columns {
                    row_ui.col(|ui| {
                        if is_selected {
                            ui.painter().rect_filled(
                                ui.max_rect(),
                                0.0,
                                ui.visuals().selection.bg_fill,
                            );
                        } else if is_hovered {
                            ui.painter().rect_filled(
                                ui.max_rect(),
                                0.0,
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        let Some(column_data) = dataset.column(&column.id) else {
                            ui.label("-");
                            return;
                        };
                        let display = display_map.get(column.id.as_str()).copied();
                        let value =
                            format_cell_value(&column_data.values, row, display, options.format);
                        let raw = column_data
                            .values
                            .display_value(row)
                            .unwrap_or_else(|| "-".into());
                        let response = ui.add(
                            egui::Label::new(&value)
                                .sense(egui::Sense::click())
                                .truncate(true),
                        );
                        let response = if raw != value {
                            response.on_hover_text(raw)
                        } else {
                            response
                        };
                        if response.clicked() {
                            output.selected_row = Some(key.clone());
                        }
                    });
                }
            });
        });

    output
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
        ui.add(egui::Slider::new(&mut settings.pixel_size, 5.0..=100.0));
        ui.label("Contrast");
        ui.add(egui::Slider::new(
            &mut settings.contrast_percentile,
            90.0..=100.0,
        ));
        egui::ComboBox::from_id_source(format!("{view_id}_colormap"))
            .selected_text(settings.colormap.label())
            .show_ui(ui, |ui| {
                for colormap in Colormap::ALL {
                    ui.selectable_value(&mut settings.colormap, colormap, colormap.label());
                }
            });
        ui.label("Zoom");
        ui.add(egui::Slider::new(&mut settings.zoom, 0.5..=8.0).logarithmic(true));
        // Zoom only affects display scaling -- exclude it from the dirty check
        // so that zooming does not trigger an expensive image re-render.
        let zoom = settings.zoom;
        settings.zoom = state.settings.zoom;
        state.set_settings(settings);
        state.settings.zoom = zoom;
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

pub fn render_image2d_view(
    ui: &mut egui::Ui,
    view_id: &str,
    state: &mut Image2dViewState,
    dataset: Option<&Image2dV1>,
    empty_message: &str,
    allow_clear: bool,
) -> HostViewUiActions {
    let mut actions = HostViewUiActions::default();

    ui.horizontal_wrapped(|ui| {
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
        ui.label("Contrast");
        ui.add(egui::Slider::new(
            &mut settings.contrast_percentile,
            90.0..=100.0,
        ));
        egui::ComboBox::from_id_source(format!("{view_id}_image_colormap"))
            .selected_text(settings.colormap.label())
            .show_ui(ui, |ui| {
                for colormap in Colormap::ALL {
                    ui.selectable_value(&mut settings.colormap, colormap, colormap.label());
                }
            });
        ui.label("Zoom");
        ui.add(egui::Slider::new(&mut settings.zoom, 0.5..=8.0).logarithmic(true));
        let zoom = settings.zoom;
        settings.zoom = state.settings.zoom;
        state.set_settings(settings);
        state.settings.zoom = zoom;
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
        ui.label(format!("Pixels: {}", dataset.pixels.len()));
        ui.separator();
        ui.label(format!(
            "Image: {} x {} px",
            state.rendered_size[0], state.rendered_size[1]
        ));
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

pub fn render_scatter2d_view(
    ui: &mut egui::Ui,
    schema: &TableSchema,
    dataset: Option<&TableDatasetV1>,
    empty_message: &str,
    options: Scatter2dViewOptions<'_>,
) -> HostViewUiActions {
    let mut actions = HostViewUiActions::default();

    ui.horizontal_wrapped(|ui| {
        if ui.button("Export CSV").clicked() {
            actions.export_csv = true;
        }
        if options.allow_clear && ui.button("Clear").clicked() {
            actions.clear_requested = true;
        }
        ui.separator();
        ui.label(format!(
            "Rows: {}",
            dataset.map(TableDatasetV1::row_count).unwrap_or(0)
        ));
        ui.separator();
        ui.label(format!(
            "Axes: {} vs {}",
            options.x_column, options.y_column
        ));
    });
    ui.separator();

    let Some(dataset) = dataset else {
        ui.label(empty_message);
        return actions;
    };

    let points = match scatter_plot_points(dataset, options.x_column, options.y_column) {
        Ok(points) => points,
        Err(err) => {
            ui.colored_label(ui.visuals().error_fg_color, err);
            return actions;
        }
    };

    let plot = scatter_plot_builder(options.view_id, schema, options.x_column, options.y_column);
    plot.show(ui, |plot_ui| {
        plot_ui.points(
            Points::new(points)
                .radius(2.0)
                .name(format!("{} vs {}", options.x_column, options.y_column)),
        );
    });

    actions
}

pub fn render_line_series_view(
    ui: &mut egui::Ui,
    view_id: &str,
    dataset: Option<&Series1dV1>,
    empty_message: &str,
) {
    let Some(dataset) = dataset else {
        ui.label(empty_message);
        return;
    };
    if dataset.is_empty() {
        ui.label(empty_message);
        return;
    }

    Plot::new(format!("{view_id}_series"))
        .legend(Legend::default())
        .height(280.0)
        .x_axis_label(&dataset.x_label)
        .y_axis_label(&dataset.y_label)
        .show(ui, |plot_ui| {
            for (index, series) in dataset.lines.iter().enumerate() {
                if series.points.is_empty() {
                    continue;
                }
                let points =
                    PlotPoints::from_iter(series.points.iter().map(|point| [point.x, point.y]));
                let name = if series.name.trim().is_empty() {
                    format!("Series {}", index + 1)
                } else {
                    series.name.clone()
                };
                plot_ui.line(Line::new(points).name(name));
            }
        });

    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.label(format!("Lines: {}", dataset.lines.len()));
        ui.separator();
        ui.label(format!("Points: {}", dataset.total_points()));
    });
}

#[derive(Clone, Default)]
pub struct TableWindowViewportData {
    pub dataset_id: String,
    pub generation: u64,
    pub schema: Arc<TableSchema>,
    pub dataset: Option<Arc<TableDatasetV1>>,
    pub filtered_rows: Arc<Vec<usize>>,
    pub table_state: InvestigationTableViewState,
    pub selected_row: Option<StableRowKey>,
    pub hovered_row: Option<StableRowKey>,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
    pub export_csv_requested: bool,
    pub selected_row_requested: Option<StableRowKey>,
    pub sort_column_requested: Option<String>,
    pub page_size_requested: Option<TablePageSize>,
    pub page_index_requested: Option<usize>,
    pub replay_origin_us: Option<u64>,
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

#[derive(Clone, Default)]
pub struct ImageWindowViewportData {
    pub texture: Option<TextureHandle>,
    pub rendered_width: usize,
    pub rendered_height: usize,
    pub settings: Image2dRenderSettings,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
    pub export_image_requested: Option<HostViewImageFormat>,
    pub clear_requested: bool,
}

#[derive(Clone, Default)]
pub struct ScatterWindowViewportData {
    pub schema: TableSchema,
    pub dataset: Option<Arc<TableDatasetV1>>,
    pub x_column: String,
    pub y_column: String,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
    pub export_csv_requested: bool,
    pub clear_requested: bool,
}

#[derive(Clone, Default)]
pub struct SeriesWindowViewportData {
    pub dataset: Option<Arc<Series1dV1>>,
    pub empty_message: String,
    pub error_message: Option<String>,
    pub close_requested: bool,
}

pub fn render_table_window_viewport(
    ui: &mut egui::Ui,
    shared: &Arc<Mutex<TableWindowViewportData>>,
) {
    let (
        dataset_id,
        generation,
        schema,
        dataset,
        filtered_rows,
        table_state,
        selected_row,
        hovered_row,
        empty_message,
        error_message,
        replay_origin_us,
    ) = {
        let data = shared.lock().expect("table viewport mutex poisoned");
        (
            data.dataset_id.clone(),
            data.generation,
            Arc::clone(&data.schema),
            data.dataset.clone(),
            Arc::clone(&data.filtered_rows),
            data.table_state.clone(),
            data.selected_row.clone(),
            data.hovered_row.clone(),
            data.empty_message.clone(),
            data.error_message.clone(),
            data.replay_origin_us,
        )
    };

    if let Some(error) = error_message {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return;
    }

    let output = render_linked_table_view(
        ui,
        &schema,
        dataset.as_deref(),
        &table_state,
        &empty_message,
        LinkedTableViewOptions {
            dataset_id: &dataset_id,
            generation,
            rows: &filtered_rows,
            selected_row: selected_row.as_ref(),
            hovered_row: hovered_row.as_ref(),
            allow_export: true,
            allow_clear: false,
            format: TableCellFormatOptions { replay_origin_us },
        },
    );
    if output.actions.export_csv {
        let mut data = shared.lock().expect("table viewport mutex poisoned");
        data.export_csv_requested = true;
    }
    if output.selected_row.is_some()
        || output.sort_column.is_some()
        || output.page_size.is_some()
        || output.page_index.is_some()
    {
        let mut data = shared.lock().expect("table viewport mutex poisoned");
        if let Some(selected_row) = output.selected_row {
            data.selected_row_requested = Some(selected_row);
        }
        if let Some(sort_column) = output.sort_column {
            data.sort_column_requested = Some(sort_column);
        }
        if let Some(page_size) = output.page_size {
            data.page_size_requested = Some(page_size);
        }
        if let Some(page_index) = output.page_index {
            data.page_index_requested = Some(page_index);
        }
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
        let mut data = shared.lock().expect("density viewport mutex poisoned");

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
                    for colormap in Colormap::ALL {
                        ui.selectable_value(
                            &mut data.settings.colormap,
                            colormap,
                            colormap.label(),
                        );
                    }
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

pub fn render_image_window_viewport(
    ui: &mut egui::Ui,
    view_id: &str,
    shared: &Arc<Mutex<ImageWindowViewportData>>,
) {
    let (texture, rendered_width, rendered_height, settings, empty_message, error_message) = {
        let mut data = shared.lock().expect("image viewport mutex poisoned");

        ui.horizontal_wrapped(|ui| {
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
            ui.label("Contrast");
            ui.add(egui::Slider::new(
                &mut data.settings.contrast_percentile,
                90.0..=100.0,
            ));
            egui::ComboBox::from_id_source(format!("{view_id}_image_viewport_colormap"))
                .selected_text(data.settings.colormap.label())
                .show_ui(ui, |ui| {
                    for colormap in Colormap::ALL {
                        ui.selectable_value(
                            &mut data.settings.colormap,
                            colormap,
                            colormap.label(),
                        );
                    }
                });
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut data.settings.zoom, 0.5..=8.0).logarithmic(true));
        });

        (
            data.texture.clone(),
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
        ui.label(format!("Image: {rendered_width} x {rendered_height} px"));
        ui.separator();
        ui.label(format!(
            "Contrast: {:.1}th percentile",
            settings.contrast_percentile
        ));
        ui.separator();
        ui.label(format!("Colormap: {}", settings.colormap.label()));
    });
}

pub fn render_scatter_window_viewport(
    ui: &mut egui::Ui,
    view_id: &str,
    shared: &Arc<Mutex<ScatterWindowViewportData>>,
) {
    let (schema, dataset, x_column, y_column, empty_message, error_message) = {
        let data = shared.lock().expect("scatter viewport mutex poisoned");
        (
            data.schema.clone(),
            data.dataset.clone(),
            data.x_column.clone(),
            data.y_column.clone(),
            data.empty_message.clone(),
            data.error_message.clone(),
        )
    };

    if let Some(error) = error_message {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return;
    }

    let actions = render_scatter2d_view(
        ui,
        &schema,
        dataset.as_deref(),
        &empty_message,
        Scatter2dViewOptions {
            view_id,
            x_column: &x_column,
            y_column: &y_column,
            allow_clear: true,
        },
    );
    let mut data = shared.lock().expect("scatter viewport mutex poisoned");
    data.export_csv_requested |= actions.export_csv;
    data.clear_requested |= actions.clear_requested;
}

pub fn render_series_window_viewport(
    ui: &mut egui::Ui,
    view_id: &str,
    shared: &Arc<Mutex<SeriesWindowViewportData>>,
) {
    let (dataset, empty_message, error_message) = {
        let data = shared.lock().expect("series viewport mutex poisoned");
        (
            data.dataset.clone(),
            data.empty_message.clone(),
            data.error_message.clone(),
        )
    };

    if let Some(error) = error_message {
        ui.colored_label(ui.visuals().error_fg_color, error);
        return;
    }

    render_line_series_view(ui, view_id, dataset.as_deref(), &empty_message);
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
                .and_then(|column| raw_cell_value(&column.values, row))
                .unwrap_or_default();
            write!(writer, "\"{}\"", value.replace('"', "\"\""))?;
        }
        writeln!(writer)?;
    }
    writer.flush()
}

/// Full-precision raw cell values for exports. UI display formatting (e.g.
/// four-digit float rounding in `display_value`) must never leak into CSV
/// output.
fn raw_cell_value(values: &TableColumnValues, row: usize) -> Option<String> {
    match values {
        TableColumnValues::U64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::I64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::F64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::String(values) => values.get(row).cloned(),
        TableColumnValues::Bool(values) => values.get(row).map(ToString::to_string),
    }
}

struct RenderedDensityImage {
    image: ColorImage,
    size: [usize; 2],
}

fn render_image2d_dataset(
    dataset: &Image2dV1,
    contrast_percentile: f32,
    colormap: Colormap,
) -> Result<ColorImage, String> {
    dataset.validate()?;

    let min_value = dataset
        .pixels
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .reduce(f32::min)
        .unwrap_or(0.0);
    let max_value = percentile_finite(&dataset.pixels, contrast_percentile).max(min_value + 1e-6);
    let scale = (max_value - min_value).max(1e-6);
    let pixels = dataset
        .pixels
        .iter()
        .map(|value| {
            let normalized = if value.is_finite() {
                ((value - min_value) / scale).clamp(0.0, 1.0)
            } else {
                0.0
            };
            colormap.lookup(normalized)
        })
        .collect();
    Ok(ColorImage {
        size: dataset.size(),
        pixels,
    })
}

fn render_density_image(
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    x_column: &str,
    y_column: &str,
    pixel_size: f64,
    contrast_percentile: f32,
    colormap: Colormap,
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
        let x_bounds = numeric_column_bounds(x_values, x_column)?;
        let y_bounds = numeric_column_bounds(y_values, y_column)?;
        match (x_bounds, y_bounds) {
            (Some((x_lo, x_hi)), Some((y_lo, y_hi))) => (x_lo, x_hi, y_lo, y_hi),
            _ => (0.0, pixel_size, 0.0, pixel_size),
        }
    };

    const MAX_IMAGE_DIMENSION: usize = 8192;
    let width =
        (((x_max - x_min).max(0.0) / pixel_size).ceil() as usize).clamp(1, MAX_IMAGE_DIMENSION);
    let height =
        (((y_max - y_min).max(0.0) / pixel_size).ceil() as usize).clamp(1, MAX_IMAGE_DIMENSION);
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
    let pixels: Vec<Color32> = bins
        .iter()
        .map(|&count| {
            let normalized = ((count as f32 / max_value).clamp(0.0, 1.0)).sqrt();
            colormap.lookup(normalized)
        })
        .collect();

    Ok(RenderedDensityImage {
        image: ColorImage {
            size: [width, height],
            pixels,
        },
        size: [width, height],
    })
}

fn numeric_column_bounds(
    column: &augur_plugin_api::TableColumnData,
    column_id: &str,
) -> Result<Option<(f64, f64)>, String> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut found = false;
    for index in 0..column.len() {
        let Some(value) = column.values.numeric_value(index) else {
            return Err(format!("column {column_id} is not numeric"));
        };
        if value < min {
            min = value;
        }
        if value > max {
            max = value;
        }
        found = true;
    }
    if found {
        Ok(Some((min, max)))
    } else {
        Ok(None)
    }
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

fn percentile_finite(values: &[f32], percentile: f32) -> f32 {
    let mut values: Vec<f32> = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    if values.is_empty() {
        return 1.0;
    }
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let last = values.len().saturating_sub(1);
    let index = ((last as f32) * percentile.clamp(0.0, 100.0) / 100.0).round() as usize;
    values[index.min(last)]
}

fn scatter_plot_builder(
    view_id: &str,
    schema: &TableSchema,
    x_column: &str,
    y_column: &str,
) -> Plot {
    let plot = Plot::new(format!("{view_id}_scatter"))
        .legend(Legend::default())
        .height(280.0)
        .x_axis_label(x_column)
        .y_axis_label(y_column);
    if schema
        .coordinate_space_2d
        .as_ref()
        .is_some_and(|space| space.x_column == x_column && space.y_column == y_column)
    {
        plot.data_aspect(1.0)
    } else {
        plot
    }
}

fn scatter_plot_points(
    dataset: &TableDatasetV1,
    x_column: &str,
    y_column: &str,
) -> Result<PlotPoints, String> {
    let x_values = dataset
        .column(x_column)
        .ok_or_else(|| format!("dataset is missing x column {x_column}"))?;
    let y_values = dataset
        .column(y_column)
        .ok_or_else(|| format!("dataset is missing y column {y_column}"))?;
    let mut points = Vec::with_capacity(dataset.row_count());
    for row in 0..dataset.row_count() {
        let Some(x) = x_values.values.numeric_value(row) else {
            return Err(format!("column {x_column} is not numeric"));
        };
        let Some(y) = y_values.values.numeric_value(row) else {
            return Err(format!("column {y_column} is not numeric"));
        };
        points.push([x, y]);
    }
    Ok(PlotPoints::new(points))
}

#[cfg(test)]
mod tests {
    use super::*;
    use augur_plugin_api::{
        HostActionDescriptor, HostActionScope, HostDatasetDescriptor, HostDatasetKind,
        HostViewDescriptor, HostViewKind, HostViewPlacement, Image2dV1, Series1dLine,
        Series1dPoint, Series1dV1, TableColumn, TableColumnData, TableColumnValues, TableDatasetV1,
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
            coordinate_space_3d: None,
            row_id_column: None,
            time_column: None,
            layer_id: None,
            semantic_label: None,
            provenance: None,
            column_display: Vec::new(),
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

    fn image_dataset() -> Image2dV1 {
        Image2dV1::new(2, 2, vec![0.0, 1.0, 2.0, 3.0]).expect("image must validate")
    }

    fn series_dataset() -> Series1dV1 {
        Series1dV1 {
            x_label: "Frame".into(),
            y_label: "Score".into(),
            lines: vec![Series1dLine {
                name: "Focus".into(),
                points: vec![
                    Series1dPoint { x: 0.0, y: 1.0 },
                    Series1dPoint { x: 1.0, y: 1.5 },
                ],
            }],
        }
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
                actions: Vec::new(),
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
                display: None,
                relations: Vec::new(),
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
                display: None,
                relations: Vec::new(),
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
            display: None,
            relations: Vec::new(),
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
    fn format_cell_value_renders_timestamp_micros_relative_to_replay_origin() {
        let values = TableColumnValues::U64(vec![1_500_000]);
        let formatted = format_cell_value(
            &values,
            0,
            Some(&TableColumnDisplayMetadata {
                format: Some(TableColumnDisplayFormat::TimestampMicros),
                ..Default::default()
            }),
            TableCellFormatOptions {
                replay_origin_us: Some(500_000),
            },
        );
        assert_eq!(formatted, "00:01.000");
    }

    #[test]
    fn format_cell_value_respects_fixed_precision() {
        let values = TableColumnValues::F64(vec![1.23456]);
        let formatted = format_cell_value(
            &values,
            0,
            Some(&TableColumnDisplayMetadata {
                format: Some(TableColumnDisplayFormat::FixedPrecision { digits: 2 }),
                ..Default::default()
            }),
            TableCellFormatOptions::default(),
        );
        assert_eq!(formatted, "1.23");
    }

    #[test]
    fn format_cell_value_falls_back_to_raw_without_metadata() {
        let values = TableColumnValues::U64(vec![42]);
        let formatted = format_cell_value(&values, 0, None, TableCellFormatOptions::default());
        assert_eq!(formatted, "42");
    }

    #[test]
    fn decode_dataset_snapshot_validates_table_schema() {
        let descriptor = HostDatasetDescriptor {
            id: "dataset.localization".into(),
            title: "Localizations".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
            display: None,
            relations: Vec::new(),
        };
        let bytes = serde_json::to_vec(&table_dataset()).expect("dataset json");

        let snapshot = decode_dataset_snapshot(&descriptor, &bytes).expect("snapshot");
        assert_eq!(
            snapshot,
            HostDatasetSnapshot::Table(Arc::new(table_dataset()))
        );
    }

    #[test]
    fn decode_dataset_snapshot_supports_image_and_series_datasets() {
        let image_descriptor = HostDatasetDescriptor {
            id: "dataset.image".into(),
            title: "Image".into(),
            kind: HostDatasetKind::Image2dV1,
            empty_message: "No image".into(),
            display: None,
            relations: Vec::new(),
        };
        let image_bytes = serde_json::to_vec(&image_dataset()).expect("image json");
        let image_snapshot =
            decode_dataset_snapshot(&image_descriptor, &image_bytes).expect("image snapshot");
        assert_eq!(
            image_snapshot,
            HostDatasetSnapshot::Image2d(Arc::new(image_dataset()))
        );

        let series_descriptor = HostDatasetDescriptor {
            id: "dataset.series".into(),
            title: "Series".into(),
            kind: HostDatasetKind::Series1dV1,
            empty_message: "No series".into(),
            display: None,
            relations: Vec::new(),
        };
        let series_bytes = serde_json::to_vec(&series_dataset()).expect("series json");
        let series_snapshot =
            decode_dataset_snapshot(&series_descriptor, &series_bytes).expect("series snapshot");
        assert_eq!(
            series_snapshot,
            HostDatasetSnapshot::Series1d(Arc::new(series_dataset()))
        );
    }

    #[test]
    fn window_views_appear_only_for_window_placements() {
        let dataset = HostDatasetDescriptor {
            id: "dataset.localization".into(),
            title: "Localizations".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
            display: None,
            relations: Vec::new(),
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
    fn resolver_surfaces_actions_whose_scope_dataset_is_resolved() {
        let dataset = HostDatasetDescriptor {
            id: "dataset.candidates".into(),
            title: "Candidates".into(),
            kind: HostDatasetKind::TableV1(table_schema()),
            empty_message: "No rows".into(),
            display: None,
            relations: Vec::new(),
        };
        let contribution = HostRegistryContribution {
            provider: HostViewProviderKey::Runtime(0),
            provider_name: "Fitting".into(),
            registry: HostViewRegistry {
                datasets: vec![dataset],
                views: Vec::new(),
                actions: vec![
                    HostActionDescriptor {
                        id: "refit_cluster".into(),
                        title: "Tune fit".into(),
                        scope: HostActionScope::Cluster {
                            dataset_id: "dataset.candidates".into(),
                            group_column: "cluster_id".into(),
                        },
                        param_schema: None,
                    },
                    HostActionDescriptor {
                        id: "orphan".into(),
                        title: "Orphan".into(),
                        scope: HostActionScope::Row {
                            dataset_id: "dataset.does_not_exist".into(),
                        },
                        param_schema: None,
                    },
                ],
            },
        };

        let resolved = resolve_host_view_registry([contribution]);
        let actions: Vec<_> = resolved
            .actions()
            .map(|a| a.descriptor.id.clone())
            .collect();
        assert_eq!(actions, vec!["refit_cluster".to_owned()]);
        assert!(resolved
            .warnings()
            .iter()
            .any(|w| w.contains("orphan") && w.contains("does_not_exist")));
    }

    #[test]
    fn incompatible_view_dataset_pairs_are_filtered_with_warning() {
        let resolved = resolve_host_view_registry([contribution(
            HostViewProviderKey::Runtime(0),
            "Provider",
            HostDatasetDescriptor {
                id: "dataset.image".into(),
                title: "Image".into(),
                kind: HostDatasetKind::Image2dV1,
                empty_message: "No image".into(),
                display: None,
                relations: Vec::new(),
            },
            HostViewDescriptor {
                id: "view.table".into(),
                title: "Table".into(),
                dataset_id: "dataset.image".into(),
                placement: HostViewPlacement::Window,
                kind: HostViewKind::TableWindow,
            },
        )]);

        assert!(resolved.window_views().next().is_none());
        assert_eq!(resolved.warnings().len(), 1);
        assert!(resolved.warnings()[0].contains("do not match dataset"));
    }

    #[test]
    fn reset_provider_for_dataset_targets_only_the_resolved_owner() {
        let dataset = ResolvedHostDataset {
            descriptor: HostDatasetDescriptor {
                id: "dataset.localization".into(),
                title: "Localizations".into(),
                kind: HostDatasetKind::TableV1(table_schema()),
                empty_message: "No rows".into(),
                display: None,
                relations: Vec::new(),
            },
            provider: HostViewProviderKey::Runtime(2),
            provider_name: "Runtime".into(),
        };

        let mut runtime_calls = Vec::new();
        reset_provider_for_dataset(&dataset, |index| runtime_calls.push(index));

        assert_eq!(runtime_calls, vec![2]);
    }
}
