use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

pub const CTX_GLOBAL_SETTINGS: &str = "augur.global_settings";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSettings {
    pub nm_per_pixel: f64,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub acq_time_ms: u64,
    pub event_store_budget_bytes: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostViewRegistry {
    pub datasets: Vec<HostDatasetDescriptor>,
    pub views: Vec<HostViewDescriptor>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostDatasetDescriptor {
    pub id: String,
    pub title: String,
    pub kind: HostDatasetKind,
    pub empty_message: String,
    pub display: Option<HostDatasetDisplayMetadata>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostViewDescriptor {
    pub id: String,
    pub title: String,
    pub dataset_id: String,
    pub placement: HostViewPlacement,
    pub kind: HostViewKind,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "schema", rename_all = "snake_case")]
pub enum HostDatasetKind {
    TableV1(TableSchema),
    Image2dV1,
    Series1dV1,
}

impl Default for HostDatasetKind {
    fn default() -> Self {
        Self::TableV1(TableSchema::default())
    }
}

impl HostDatasetKind {
    pub fn table_schema(&self) -> Option<&TableSchema> {
        match self {
            Self::TableV1(schema) => Some(schema),
            Self::Image2dV1 | Self::Series1dV1 => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostViewPlacement {
    AnalysisPanel,
    Window,
}

impl Default for HostViewPlacement {
    fn default() -> Self {
        Self::AnalysisPanel
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostViewKind {
    CompactTable,
    TableWindow,
    Density2dFromTable {
        x_column: String,
        y_column: String,
    },
    Scatter2dFromTable {
        x_column: String,
        y_column: String,
    },
    Scatter3dFromTable {
        x_column: String,
        y_column: String,
        z_column: String,
    },
    ImageWindow,
    LineSeriesWindow,
}

impl Default for HostViewKind {
    fn default() -> Self {
        Self::CompactTable
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableSchema {
    pub columns: Vec<TableColumn>,
    pub coordinate_space_2d: Option<TableCoordinateSpace2d>,
    pub coordinate_space_3d: Option<TableCoordinateSpace3d>,
    pub row_id_column: Option<String>,
    pub time_column: Option<String>,
    pub layer_id: Option<String>,
    pub semantic_label: Option<String>,
}

impl TableSchema {
    pub fn column(&self, id: &str) -> Option<&TableColumn> {
        self.columns.iter().find(|column| column.id == id)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableColumn {
    pub id: String,
    pub title: String,
    pub value_type: TableValueType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableValueType {
    U64,
    I64,
    F64,
    String,
    Bool,
}

impl Default for TableValueType {
    fn default() -> Self {
        Self::F64
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TableCoordinateSpace2d {
    pub x_column: String,
    pub y_column: String,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl<'de> Deserialize<'de> for TableCoordinateSpace2d {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawCoordinateSpace2d {
            x_column: String,
            y_column: String,
            x_min: f64,
            x_max: f64,
            y_min: f64,
            y_max: f64,
        }

        let raw = RawCoordinateSpace2d::deserialize(deserializer)?;
        if raw.x_min > raw.x_max {
            return Err(D::Error::custom(
                "x_min must be less than or equal to x_max",
            ));
        }
        if raw.y_min > raw.y_max {
            return Err(D::Error::custom(
                "y_min must be less than or equal to y_max",
            ));
        }

        Ok(Self {
            x_column: raw.x_column,
            y_column: raw.y_column,
            x_min: raw.x_min,
            x_max: raw.x_max,
            y_min: raw.y_min,
            y_max: raw.y_max,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TableCoordinateSpace3d {
    pub x_column: String,
    pub y_column: String,
    pub z_column: String,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub z_min: f64,
    pub z_max: f64,
}

impl<'de> Deserialize<'de> for TableCoordinateSpace3d {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawCoordinateSpace3d {
            x_column: String,
            y_column: String,
            z_column: String,
            x_min: f64,
            x_max: f64,
            y_min: f64,
            y_max: f64,
            z_min: f64,
            z_max: f64,
        }

        let raw = RawCoordinateSpace3d::deserialize(deserializer)?;
        if raw.x_min > raw.x_max {
            return Err(D::Error::custom(
                "x_min must be less than or equal to x_max",
            ));
        }
        if raw.y_min > raw.y_max {
            return Err(D::Error::custom(
                "y_min must be less than or equal to y_max",
            ));
        }
        if raw.z_min > raw.z_max {
            return Err(D::Error::custom(
                "z_min must be less than or equal to z_max",
            ));
        }

        Ok(Self {
            x_column: raw.x_column,
            y_column: raw.y_column,
            z_column: raw.z_column,
            x_min: raw.x_min,
            x_max: raw.x_max,
            y_min: raw.y_min,
            y_max: raw.y_max,
            z_min: raw.z_min,
            z_max: raw.z_max,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostMarkerShape {
    Circle,
    Square,
    Diamond,
    Cross,
    Point,
    Box,
    Ellipse,
    FilledCircle,
}

impl Default for HostMarkerShape {
    fn default() -> Self {
        Self::Circle
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostDatasetDisplayMetadata {
    pub layer_title: Option<String>,
    pub default_visibility: Option<bool>,
    pub default_color: Option<[u8; 4]>,
    pub default_marker_shape: Option<HostMarkerShape>,
    pub default_size: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TableDatasetV1 {
    pub columns: Vec<TableColumnData>,
}

impl TableDatasetV1 {
    pub fn new(columns: Vec<TableColumnData>) -> Result<Self, String> {
        let expected = columns.first().map(TableColumnData::len).unwrap_or(0);
        if columns.iter().any(|column| column.len() != expected) {
            return Err("all table columns must have identical lengths".into());
        }
        Ok(Self { columns })
    }

    pub fn row_count(&self) -> usize {
        self.columns.first().map(TableColumnData::len).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.row_count() == 0
    }

    pub fn column(&self, id: &str) -> Option<&TableColumnData> {
        self.columns.iter().find(|column| column.column_id == id)
    }

    pub fn validate_against_schema(&self, schema: &TableSchema) -> Result<(), String> {
        if self.columns.len() != schema.columns.len() {
            return Err(format!(
                "dataset column count {} does not match schema column count {}",
                self.columns.len(),
                schema.columns.len()
            ));
        }

        for schema_column in &schema.columns {
            let Some(column) = self.column(&schema_column.id) else {
                return Err(format!(
                    "dataset is missing schema column {}",
                    schema_column.id
                ));
            };
            if column.values.value_type() != schema_column.value_type {
                return Err(format!(
                    "dataset column {} has type {:?}, expected {:?}",
                    schema_column.id,
                    column.values.value_type(),
                    schema_column.value_type
                ));
            }
        }

        for column in &self.columns {
            if schema.column(&column.column_id).is_none() {
                return Err(format!(
                    "dataset contains unknown column {}",
                    column.column_id
                ));
            }
        }

        Ok(())
    }
}

impl<'de> Deserialize<'de> for TableDatasetV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawTableDatasetV1 {
            columns: Vec<TableColumnData>,
        }

        let raw = RawTableDatasetV1::deserialize(deserializer)?;
        Self::new(raw.columns).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableColumnData {
    pub column_id: String,
    pub values: TableColumnValues,
}

impl TableColumnData {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "value_type", content = "values", rename_all = "snake_case")]
pub enum TableColumnValues {
    U64(Vec<u64>),
    I64(Vec<i64>),
    F64(Vec<f64>),
    String(Vec<String>),
    Bool(Vec<bool>),
}

impl TableColumnValues {
    pub fn len(&self) -> usize {
        match self {
            Self::U64(values) => values.len(),
            Self::I64(values) => values.len(),
            Self::F64(values) => values.len(),
            Self::String(values) => values.len(),
            Self::Bool(values) => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn value_type(&self) -> TableValueType {
        match self {
            Self::U64(_) => TableValueType::U64,
            Self::I64(_) => TableValueType::I64,
            Self::F64(_) => TableValueType::F64,
            Self::String(_) => TableValueType::String,
            Self::Bool(_) => TableValueType::Bool,
        }
    }

    pub fn numeric_value(&self, index: usize) -> Option<f64> {
        match self {
            Self::U64(values) => values.get(index).copied().map(|v| v as f64),
            Self::I64(values) => values.get(index).copied().map(|v| v as f64),
            Self::F64(values) => values.get(index).copied(),
            Self::String(_) | Self::Bool(_) => None,
        }
    }

    pub fn display_value(&self, index: usize) -> Option<String> {
        match self {
            Self::U64(values) => values.get(index).map(ToString::to_string),
            Self::I64(values) => values.get(index).map(ToString::to_string),
            Self::F64(values) => values.get(index).map(|value| format!("{value:.4}")),
            Self::String(values) => values.get(index).cloned(),
            Self::Bool(values) => values.get(index).map(ToString::to_string),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Image2dV1 {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
}

impl Image2dV1 {
    pub fn new(width: u32, height: u32, pixels: Vec<f32>) -> Result<Self, String> {
        let image = Self {
            width,
            height,
            pixels,
        };
        image.validate()?;
        Ok(image)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err("image dimensions must be greater than zero".into());
        }

        let expected = self.width as usize * self.height as usize;
        if self.pixels.len() != expected {
            return Err(format!(
                "image pixel count {} does not match {}x{} dimensions",
                self.pixels.len(),
                self.width,
                self.height
            ));
        }

        Ok(())
    }

    pub fn size(&self) -> [usize; 2] {
        [self.width as usize, self.height as usize]
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }
}

impl<'de> Deserialize<'de> for Image2dV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawImage2dV1 {
            width: u32,
            height: u32,
            pixels: Vec<f32>,
        }

        let raw = RawImage2dV1::deserialize(deserializer)?;
        Self::new(raw.width, raw.height, raw.pixels).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Series1dPoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Series1dLine {
    pub name: String,
    pub points: Vec<Series1dPoint>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Series1dV1 {
    pub x_label: String,
    pub y_label: String,
    pub lines: Vec<Series1dLine>,
}

impl Series1dV1 {
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|line| line.points.is_empty())
    }

    pub fn total_points(&self) -> usize {
        self.lines.iter().map(|line| line.points.len()).sum()
    }
}
