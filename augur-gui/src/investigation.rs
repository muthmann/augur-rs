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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationTableViewState {
    pub sort_column: Option<String>,
    pub sort_direction: InvestigationSortDirection,
}

impl Default for InvestigationTableViewState {
    fn default() -> Self {
        Self {
            sort_column: None,
            sort_direction: InvestigationSortDirection::Ascending,
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
