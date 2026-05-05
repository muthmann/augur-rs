use std::collections::{BTreeSet, HashMap};

use augur_core::config::RoiConfig;
use augur_plugin_api::{
    HostDatasetDescriptor, HostMarkerShape, TableColumnValues, TableDatasetV1, TableSchema,
};

const DEFAULT_LAYER_COLOR: [u8; 4] = [244, 244, 244, 255];
const DEFAULT_LAYER_SIZE: f32 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestigationLayout {
    Preview2dOnly,
    Split2d3d,
    Inspection3dOnly,
}

impl InvestigationLayout {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preview2dOnly => "2D only",
            Self::Split2d3d => "Split 2D + 3D",
            Self::Inspection3dOnly => "3D only",
        }
    }

    pub fn shows_2d(self) -> bool {
        matches!(self, Self::Preview2dOnly | Self::Split2d3d)
    }

    pub fn shows_3d(self) -> bool {
        matches!(self, Self::Split2d3d | Self::Inspection3dOnly)
    }
}

impl Default for InvestigationLayout {
    fn default() -> Self {
        Self::Split2d3d
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisRoi {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl AnalysisRoi {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x_min && x <= self.x_max && y >= self.y_min && y <= self.y_max
    }

    pub fn from_sensor_roi(roi: RoiConfig) -> Self {
        Self {
            x_min: f64::from(roi.x),
            x_max: f64::from(roi.x.saturating_add(roi.width.saturating_sub(1))),
            y_min: f64::from(roi.y),
            y_max: f64::from(roi.y.saturating_add(roi.height.saturating_sub(1))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableRowKey {
    pub dataset_id: String,
    pub row_id: String,
}

impl StableRowKey {
    pub fn new(dataset_id: impl Into<String>, row_id: impl Into<String>) -> Self {
        Self {
            dataset_id: dataset_id.into(),
            row_id: row_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvestigationSortDirection {
    Ascending,
    Descending,
}

impl InvestigationSortDirection {
    pub fn toggled(self) -> Self {
        match self {
            Self::Ascending => Self::Descending,
            Self::Descending => Self::Ascending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TablePageSize {
    Rows25,
    Rows100,
    Rows500,
    All,
}

impl Default for TablePageSize {
    fn default() -> Self {
        Self::Rows100
    }
}

impl TablePageSize {
    pub const ALL: [TablePageSize; 4] = [Self::Rows25, Self::Rows100, Self::Rows500, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rows25 => "25",
            Self::Rows100 => "100",
            Self::Rows500 => "500",
            Self::All => "All",
        }
    }

    pub fn rows_per_page(self, total: usize) -> usize {
        match self {
            Self::Rows25 => 25,
            Self::Rows100 => 100,
            Self::Rows500 => 500,
            Self::All => total.max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationTableViewState {
    pub sort_column: Option<String>,
    pub sort_direction: InvestigationSortDirection,
    pub page_size: TablePageSize,
    pub page_index: usize,
}

impl Default for InvestigationTableViewState {
    fn default() -> Self {
        Self {
            sort_column: None,
            sort_direction: InvestigationSortDirection::Ascending,
            page_size: TablePageSize::default(),
            page_index: 0,
        }
    }
}

impl InvestigationTableViewState {
    pub fn toggle_sort(&mut self, column_id: &str) {
        if self.sort_column.as_deref() == Some(column_id) {
            self.sort_direction = self.sort_direction.toggled();
        } else {
            self.sort_column = Some(column_id.to_owned());
            self.sort_direction = InvestigationSortDirection::Ascending;
        }
        self.page_index = 0;
    }

    pub fn page_count(&self, total_rows: usize) -> usize {
        if total_rows == 0 {
            return 1;
        }
        let per_page = self.page_size.rows_per_page(total_rows).max(1);
        (total_rows + per_page - 1) / per_page
    }

    pub fn clamp_page(&mut self, total_rows: usize) {
        let max_index = self.page_count(total_rows).saturating_sub(1);
        if self.page_index > max_index {
            self.page_index = max_index;
        }
    }

    pub fn visible_slice(&self, total_rows: usize) -> (usize, usize) {
        let per_page = self.page_size.rows_per_page(total_rows).max(1);
        let start = self.page_index * per_page;
        let end = (start + per_page).min(total_rows);
        (start, end)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvestigationLayerStyle {
    pub title: String,
    pub visible: bool,
    pub color: [u8; 4],
    pub marker_shape: HostMarkerShape,
    pub size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Investigation2dPoint {
    pub position: [f64; 2],
    pub color: [u8; 4],
    pub marker_shape: HostMarkerShape,
    pub size: f32,
    pub item_key: StableRowKey,
    pub label: String,
    pub layer_id: String,
}

impl InvestigationLayerStyle {
    pub fn from_dataset(descriptor: &HostDatasetDescriptor, schema: Option<&TableSchema>) -> Self {
        let display = descriptor.display.as_ref();
        Self {
            title: display
                .and_then(|display| display.layer_title.clone())
                .or_else(|| schema.and_then(|schema| schema.semantic_label.clone()))
                .unwrap_or_else(|| descriptor.title.clone()),
            visible: display
                .and_then(|display| display.default_visibility)
                .unwrap_or(true),
            color: display
                .and_then(|display| display.default_color)
                .unwrap_or(DEFAULT_LAYER_COLOR),
            marker_shape: display
                .and_then(|display| display.default_marker_shape)
                .unwrap_or(HostMarkerShape::Circle),
            size: display
                .and_then(|display| display.default_size)
                .unwrap_or(DEFAULT_LAYER_SIZE),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InvestigationState {
    pub layout: InvestigationLayout,
    pub split_ratio: f32,
    pub active_analysis_roi: Option<AnalysisRoi>,
    pub selected_rows: BTreeSet<StableRowKey>,
    pub hovered_row: Option<StableRowKey>,
    pub focused_layers: BTreeSet<String>,
    pub link_roi_between_2d_and_3d: bool,
    pub camera_focus_target: Option<[f32; 3]>,
    pub table_views: HashMap<String, InvestigationTableViewState>,
    pub layer_visibility: HashMap<String, bool>,
    pub layer_styles: HashMap<String, InvestigationLayerStyle>,
}

impl Default for InvestigationState {
    fn default() -> Self {
        Self {
            layout: InvestigationLayout::default(),
            split_ratio: 0.56,
            active_analysis_roi: None,
            selected_rows: BTreeSet::new(),
            hovered_row: None,
            focused_layers: BTreeSet::new(),
            link_roi_between_2d_and_3d: true,
            camera_focus_target: None,
            table_views: HashMap::new(),
            layer_visibility: HashMap::new(),
            layer_styles: HashMap::new(),
        }
    }
}

impl InvestigationState {
    pub fn primary_selection(&self) -> Option<&StableRowKey> {
        self.selected_rows.iter().next()
    }

    pub fn set_single_selection(&mut self, key: StableRowKey) {
        self.selected_rows.clear();
        self.selected_rows.insert(key);
    }

    pub fn clear_selection(&mut self) {
        self.selected_rows.clear();
    }

    pub fn table_view_state_mut(&mut self, dataset_id: &str) -> &mut InvestigationTableViewState {
        self.table_views.entry(dataset_id.to_owned()).or_default()
    }

    pub fn layer_visible(&self, layer_id: &str, fallback: bool) -> bool {
        self.layer_visibility
            .get(layer_id)
            .copied()
            .unwrap_or(fallback)
    }

    pub fn set_layer_visible(&mut self, layer_id: impl Into<String>, visible: bool) {
        self.layer_visibility.insert(layer_id.into(), visible);
    }

    pub fn isolate_layer(&mut self, layer_id: &str) {
        self.focused_layers.clear();
        self.focused_layers.insert(layer_id.to_owned());
        let all_layer_ids: Vec<String> = self.layer_styles.keys().cloned().collect();
        for candidate in all_layer_ids {
            self.set_layer_visible(candidate.clone(), candidate == layer_id);
        }
    }

    pub fn upsert_authoritative_layer(
        &mut self,
        layer_id: impl Into<String>,
        mut style: InvestigationLayerStyle,
    ) {
        let layer_id = layer_id.into();
        if let Some(existing) = self.layer_styles.get(&layer_id) {
            style.visible = existing.visible;
        }
        self.layer_styles.insert(layer_id, style);
    }

    pub fn sync_dataset_layer(
        &mut self,
        descriptor: &HostDatasetDescriptor,
        schema: Option<&TableSchema>,
    ) -> String {
        let layer_id = dataset_layer_id(descriptor, schema);
        let style = self
            .layer_styles
            .entry(layer_id.clone())
            .or_insert_with(|| InvestigationLayerStyle::from_dataset(descriptor, schema))
            .clone();
        self.layer_visibility
            .entry(layer_id.clone())
            .or_insert(style.visible);
        layer_id
    }
}

pub fn dataset_layer_id(
    descriptor: &HostDatasetDescriptor,
    schema: Option<&TableSchema>,
) -> String {
    schema
        .and_then(|schema| schema.layer_id.clone())
        .unwrap_or_else(|| descriptor.id.clone())
}

pub fn row_key_for_row(
    dataset_id: &str,
    generation: u64,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    row: usize,
) -> StableRowKey {
    let row_id = schema
        .row_id_column
        .as_deref()
        .and_then(|column_id| dataset.column(column_id))
        .and_then(|column| stable_row_id_value(&column.values, row))
        .unwrap_or_else(|| format!("gen:{generation}:row:{row}"));
    StableRowKey::new(dataset_id, row_id)
}

pub fn stable_row_id_value(values: &TableColumnValues, index: usize) -> Option<String> {
    match values {
        TableColumnValues::U64(values) => values.get(index).map(ToString::to_string),
        TableColumnValues::I64(values) => values.get(index).map(ToString::to_string),
        TableColumnValues::String(values) => values.get(index).cloned(),
        TableColumnValues::F64(_) | TableColumnValues::Bool(_) => None,
    }
}

pub fn coordinate_2d_for_row(
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    row: usize,
) -> Option<[f64; 2]> {
    let space = schema.coordinate_space_2d.as_ref()?;
    Some([
        dataset.column(&space.x_column)?.values.numeric_value(row)?,
        dataset.column(&space.y_column)?.values.numeric_value(row)?,
    ])
}

pub fn coordinate_3d_for_row(
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    row: usize,
    columns: Option<(&str, &str, &str)>,
) -> Option<[f64; 3]> {
    let (x_column, y_column, z_column) = columns
        .map(|(x, y, z)| (x.to_owned(), y.to_owned(), z.to_owned()))
        .or_else(|| {
            schema.coordinate_space_3d.as_ref().map(|space| {
                (
                    space.x_column.clone(),
                    space.y_column.clone(),
                    space.z_column.clone(),
                )
            })
        })?;
    Some([
        dataset.column(&x_column)?.values.numeric_value(row)?,
        dataset.column(&y_column)?.values.numeric_value(row)?,
        dataset.column(&z_column)?.values.numeric_value(row)?,
    ])
}

pub fn filtered_row_indices(
    state: &InvestigationState,
    dataset_id: &str,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
) -> Vec<usize> {
    let mut rows: Vec<usize> = (0..dataset.row_count())
        .filter(|row| row_matches_roi(state.active_analysis_roi.as_ref(), schema, dataset, *row))
        .collect();

    if let Some(table_state) = state.table_views.get(dataset_id) {
        if let Some(column_id) = table_state.sort_column.as_deref() {
            rows.sort_by(|left, right| {
                let ordering = compare_rows_by_column(dataset, column_id, *left, *right);
                match table_state.sort_direction {
                    InvestigationSortDirection::Ascending => ordering,
                    InvestigationSortDirection::Descending => ordering.reverse(),
                }
            });
        }
    }

    rows
}

/// Retain only rows whose declared time span overlaps the given frame window.
/// Rows without declared provenance (i.e. no resolvable span) are kept so
/// plugins that have not opted into provenance remain visible.
pub fn retain_rows_in_frame_span(
    rows: Vec<usize>,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    window: (u64, u64),
) -> Vec<usize> {
    let (window_start, window_end) = window;
    rows.into_iter()
        .filter(|row| match schema.row_span_us(dataset, *row) {
            Some((start, end)) => start <= window_end && end >= window_start,
            None => true,
        })
        .collect()
}

pub fn row_matches_roi(
    roi: Option<&AnalysisRoi>,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
    row: usize,
) -> bool {
    let Some(roi) = roi else {
        return true;
    };
    let Some([x, y]) = coordinate_2d_for_row(schema, dataset, row) else {
        return true;
    };
    roi.contains(x, y)
}

fn compare_rows_by_column(
    dataset: &TableDatasetV1,
    column_id: &str,
    left_row: usize,
    right_row: usize,
) -> std::cmp::Ordering {
    let Some(column) = dataset.column(column_id) else {
        return std::cmp::Ordering::Equal;
    };

    match &column.values {
        TableColumnValues::U64(values) => values.get(left_row).cmp(&values.get(right_row)),
        TableColumnValues::I64(values) => values.get(left_row).cmp(&values.get(right_row)),
        TableColumnValues::F64(values) => values
            .get(left_row)
            .partial_cmp(&values.get(right_row))
            .unwrap_or(std::cmp::Ordering::Equal),
        TableColumnValues::String(values) => values.get(left_row).cmp(&values.get(right_row)),
        TableColumnValues::Bool(values) => values.get(left_row).cmp(&values.get(right_row)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use augur_core::config::RoiConfig;
    use augur_plugin_api::{TableColumn, TableColumnData, TableValueType};

    fn schema() -> TableSchema {
        TableSchema {
            columns: vec![
                TableColumn {
                    id: "id".into(),
                    title: "Id".into(),
                    value_type: TableValueType::String,
                },
                TableColumn {
                    id: "x".into(),
                    title: "X".into(),
                    value_type: TableValueType::F64,
                },
                TableColumn {
                    id: "y".into(),
                    title: "Y".into(),
                    value_type: TableValueType::F64,
                },
                TableColumn {
                    id: "z".into(),
                    title: "Z".into(),
                    value_type: TableValueType::F64,
                },
            ],
            coordinate_space_2d: Some(augur_plugin_api::TableCoordinateSpace2d {
                x_column: "x".into(),
                y_column: "y".into(),
                x_min: 0.0,
                x_max: 20.0,
                y_min: 0.0,
                y_max: 20.0,
            }),
            coordinate_space_3d: Some(augur_plugin_api::TableCoordinateSpace3d {
                x_column: "x".into(),
                y_column: "y".into(),
                z_column: "z".into(),
                x_min: 0.0,
                x_max: 20.0,
                y_min: 0.0,
                y_max: 20.0,
                z_min: 0.0,
                z_max: 20.0,
            }),
            row_id_column: Some("id".into()),
            time_column: None,
            layer_id: Some("layer.a".into()),
            semantic_label: Some("points".into()),
            provenance: None,
            column_display: Vec::new(),
        }
    }

    fn dataset() -> TableDatasetV1 {
        TableDatasetV1::new(vec![
            TableColumnData {
                column_id: "id".into(),
                values: TableColumnValues::String(vec!["a".into(), "b".into()]),
            },
            TableColumnData {
                column_id: "x".into(),
                values: TableColumnValues::F64(vec![2.0, 18.0]),
            },
            TableColumnData {
                column_id: "y".into(),
                values: TableColumnValues::F64(vec![3.0, 19.0]),
            },
            TableColumnData {
                column_id: "z".into(),
                values: TableColumnValues::F64(vec![4.0, 17.0]),
            },
        ])
        .expect("dataset must validate")
    }

    #[test]
    fn row_keys_use_stable_id_column_when_available() {
        let key = row_key_for_row("table.points", 7, &schema(), &dataset(), 1);
        assert_eq!(key.dataset_id, "table.points");
        assert_eq!(key.row_id, "b");
    }

    #[test]
    fn span_filter_keeps_rows_overlapping_frame_window() {
        use augur_plugin_api::TableRowProvenance;
        let mut schema = schema();
        schema.columns.push(TableColumn {
            id: "span_start".into(),
            title: "Span Start".into(),
            value_type: TableValueType::U64,
        });
        schema.columns.push(TableColumn {
            id: "span_end".into(),
            title: "Span End".into(),
            value_type: TableValueType::U64,
        });
        schema.provenance = Some(TableRowProvenance {
            anchor_time_column: None,
            span_start_column: Some("span_start".into()),
            span_end_column: Some("span_end".into()),
            anchor_frame_column: None,
        });
        let dataset = TableDatasetV1::new(vec![
            TableColumnData {
                column_id: "id".into(),
                values: TableColumnValues::String(vec!["a".into(), "b".into()]),
            },
            TableColumnData {
                column_id: "x".into(),
                values: TableColumnValues::F64(vec![2.0, 18.0]),
            },
            TableColumnData {
                column_id: "y".into(),
                values: TableColumnValues::F64(vec![3.0, 19.0]),
            },
            TableColumnData {
                column_id: "z".into(),
                values: TableColumnValues::F64(vec![4.0, 17.0]),
            },
            TableColumnData {
                column_id: "span_start".into(),
                values: TableColumnValues::U64(vec![100, 500]),
            },
            TableColumnData {
                column_id: "span_end".into(),
                values: TableColumnValues::U64(vec![200, 600]),
            },
        ])
        .expect("dataset must validate");

        // Window fully inside row 0's span only.
        let kept = retain_rows_in_frame_span(vec![0, 1], &schema, &dataset, (150, 180));
        assert_eq!(kept, vec![0]);

        // Window covers both rows.
        let kept = retain_rows_in_frame_span(vec![0, 1], &schema, &dataset, (50, 1000));
        assert_eq!(kept, vec![0, 1]);

        // Window after both spans.
        let kept = retain_rows_in_frame_span(vec![0, 1], &schema, &dataset, (700, 800));
        assert!(kept.is_empty());
    }

    #[test]
    fn span_filter_keeps_rows_without_provenance() {
        // No provenance, no time column → all rows kept regardless of window.
        let kept = retain_rows_in_frame_span(vec![0, 1], &schema(), &dataset(), (0, 1));
        assert_eq!(kept, vec![0, 1]);
    }

    #[test]
    fn filtered_rows_follow_active_roi() {
        let state = InvestigationState {
            active_analysis_roi: Some(AnalysisRoi {
                x_min: 0.0,
                x_max: 10.0,
                y_min: 0.0,
                y_max: 10.0,
            }),
            ..InvestigationState::default()
        };
        let rows = filtered_row_indices(&state, "table.points", &schema(), &dataset());
        assert_eq!(rows, vec![0]);
    }

    #[test]
    fn authoritative_layer_overwrites_color_but_preserves_visibility() {
        let mut state = InvestigationState::default();

        // Simulate a prior frame where a dataset-derived sync (or a stale
        // entry) wrote a default-coloured style into the raw ON layer id
        // and the user had toggled the raw ON layer off.
        state.layer_styles.insert(
            "host.raw_events.on".into(),
            InvestigationLayerStyle {
                title: "stale".into(),
                visible: false,
                color: DEFAULT_LAYER_COLOR,
                marker_shape: HostMarkerShape::Circle,
                size: 1.0,
            },
        );

        let raw_on_color = [255, 186, 92, 240];
        state.upsert_authoritative_layer(
            "host.raw_events.on",
            InvestigationLayerStyle {
                title: "Raw Events ON".into(),
                visible: true,
                color: raw_on_color,
                marker_shape: HostMarkerShape::Point,
                size: 2.0,
            },
        );

        let style = state
            .layer_styles
            .get("host.raw_events.on")
            .expect("raw ON style must exist");
        assert_eq!(
            style.color, raw_on_color,
            "raw ON color must not fall back to DEFAULT_LAYER_COLOR"
        );
        assert_ne!(style.color, DEFAULT_LAYER_COLOR);
        assert_eq!(style.marker_shape, HostMarkerShape::Point);
        assert!(
            !style.visible,
            "prior user-chosen visibility must be preserved across authoritative seeding"
        );
    }

    #[test]
    fn sensor_roi_conversion_uses_inclusive_max_bounds() {
        let roi = AnalysisRoi::from_sensor_roi(RoiConfig {
            x: 10,
            y: 20,
            width: 5,
            height: 7,
        });

        assert_eq!(roi.x_min, 10.0);
        assert_eq!(roi.x_max, 14.0);
        assert_eq!(roi.y_min, 20.0);
        assert_eq!(roi.y_max, 26.0);
    }
}
