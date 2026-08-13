use std::collections::BTreeMap;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

/// A semantic, target-defined service request routed by the host between
/// worker-owned plugin instances.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginServiceRequest {
    pub request_id: u64,
    #[serde(default)]
    pub source_plugin_id: String,
    pub target_plugin_id: String,
    pub service: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PluginServiceOutcome {
    Accepted {
        #[serde(default)]
        payload: serde_json::Value,
    },
    Rejected {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginServiceReply {
    pub request_id: u64,
    pub source_plugin_id: String,
    pub target_plugin_id: String,
    pub service: String,
    pub outcome: PluginServiceOutcome,
}

/// Small versioned state published by one plugin for peer monitoring.
/// High-rate or bulk sample arrays belong in plugin-owned files/datasets,
/// not in this JSON control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PluginControlSnapshot {
    #[serde(default)]
    pub plugin_id: String,
    pub topic: String,
    pub revision: u64,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum HostCommand {
    StartRecording {
        run_id: String,
        base_path: String,
        #[serde(default)]
        metadata: BTreeMap<String, String>,
    },
    StopRecording,
    /// Apply a complete host-camera configuration inside an exclusive session.
    /// The first call preserves the active configuration for a later restore;
    /// subsequent calls from the same plugin update the session without losing
    /// that original state.
    ApplyCameraConfiguration {
        configuration: CameraConfigurationSourceV1,
    },
    /// Restore the host configuration that was active before this plugin's
    /// configuration session began. Only the owning plugin may restore it.
    RestoreCameraConfiguration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostCommandRequest {
    pub request_id: u64,
    pub command: HostCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum HostCommandOutcome {
    RecordingStarted {
        actual_raw_path: String,
        started_at: String,
    },
    RecordingFinalized {
        actual_raw_path: String,
        size: u64,
        sha256: String,
        duration_us: u64,
    },
    /// The host stopped the pipeline, but at least one finalization step
    /// failed. The actual path is always reported; size/hash are present when
    /// the file remained readable so callers can retain or quarantine it.
    RecordingPartial {
        actual_raw_path: String,
        size: Option<u64>,
        sha256: Option<String>,
        duration_us: u64,
        reason: String,
    },
    CameraConfigurationApplied {
        snapshot: CameraConfigurationSnapshotV1,
        provenance: CameraConfigurationProvenanceV1,
        readback: SensorBiasReadbackV1,
        readback_age_s: f64,
    },
    CameraConfigurationRestored {
        readback: SensorBiasReadbackV1,
        readback_age_s: f64,
    },
    Rejected {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostCommandReply {
    pub request_id: u64,
    pub outcome: HostCommandOutcome,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PluginControlInbox {
    #[serde(default)]
    pub service_replies: Vec<PluginServiceReply>,
    #[serde(default)]
    pub host_replies: Vec<HostCommandReply>,
    #[serde(default)]
    pub snapshots: Vec<PluginControlSnapshot>,
}

pub const CTX_GLOBAL_SETTINGS: &str = "augur.global_settings";

/// Per-frame context bus key carrying [`SensorMonitoringV1`]. Host-written and
/// read-only for plugins. Absent whenever the host is not streaming from a
/// camera that can measure these values — see the type's docs.
pub const CTX_SENSOR_MONITORING: &str = "augur.sensor_monitoring";
/// Reserved JSON key inside [`HostActionRequest::params`] that the host uses
/// to attach raw table-row snapshots for cluster-scoped actions.
pub const HOST_ACTION_CLUSTER_ROWS_PARAM: &str = "__augur_cluster_rows";

/// Persistent context bus key where the host publishes pending
/// [`HostActionRequest`]s for plugins to consume. Single-writer: only the
/// host appends or clears this queue; plugins treat it as read-only and
/// dedupe by monotonic `request_id`.
pub const CTX_INVESTIGATION_ACTION_REQUESTS: &str = "augur.investigation.action_requests";

/// Active region of interest in sensor pixel coordinates, mirrored from the
/// host camera config so plugins can restrict analysis to the illuminated
/// window. A zero `width`/`height` means "use the full sensor".
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RoiV1 {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalSettings {
    pub nm_per_pixel: f64,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub acq_time_ms: u64,
    pub event_store_budget_bytes: usize,
    /// Whether live recordings persist the sensor-monitoring companion file.
    /// Older hosts omit it and therefore retain the safe disabled default.
    #[serde(default)]
    pub record_sensor_telemetry: bool,
    /// Active ROI from the host camera config.
    #[serde(default)]
    pub roi: RoiV1,
    /// Masked (dead/hot) pixels from the host camera config.
    #[serde(default)]
    pub masked_pixels: Vec<(u16, u16)>,
    /// Sensor-side event-shaping stages that change *which* events reach the
    /// stream at all.
    #[serde(default)]
    pub event_filters: EventFiltersV1,
}

/// State of the on-sensor filters that discard events before they are streamed.
/// Plugins can use this host-owned state to decide whether the active camera
/// configuration is compatible with their own workflow.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EventFiltersV1 {
    /// Spatio-temporal contrast filter.
    pub stc_enabled: bool,
    /// Trail filter. Shares the on-chip block with STC, so the two are
    /// mutually exclusive.
    pub trail_enabled: bool,
    /// Event-rate controller. Always `false` on hosts that do not implement one.
    pub erc_enabled: bool,
}

/// Absolute 8-bit bias codes as programmed on the sensor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SensorBiasCodesV1 {
    pub diff_on: u8,
    pub diff_off: u8,
    pub fo: u8,
    pub hpf: u8,
    pub refr: u8,
}

/// Source for a complete host-owned camera configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CameraConfigurationSourceV1 {
    /// Preserve and confirm the configuration currently active in the host.
    Current,
    /// Resolve a named host profile at apply time.
    NamedProfile { name: String },
    /// Apply an immutable, versioned snapshot supplied by the plugin.
    Snapshot {
        snapshot: CameraConfigurationSnapshotV1,
    },
}

/// Versioned host-owned camera/global settings snapshot. It intentionally
/// contains no plugin-local settings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CameraConfigurationSnapshotV1 {
    pub schema_version: u32,
    pub biases: CameraBiasOffsetsV1,
    pub roi: RoiV1,
    #[serde(default)]
    pub masked_pixels: Vec<(u16, u16)>,
    pub digital_filter: CameraDigitalFilterV1,
    #[serde(default)]
    pub external_trigger: CameraExternalTriggerV1,
    pub global: CameraGlobalSettingsV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CameraBiasOffsetsV1 {
    pub diff_on: i32,
    pub diff_off: i32,
    pub fo: i32,
    pub hpf: i32,
    pub refr: i32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CameraDigitalFilterV1 {
    pub stc_enabled: bool,
    pub stc_threshold_us: u32,
    pub trail_enabled: bool,
    /// Event-rate controller state. `None` means the host did not report it;
    /// scientific consumers must not interpret an older missing field as OFF.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub erc_enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CameraExternalTriggerV1 {
    pub enabled: bool,
    pub channel: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CameraGlobalSettingsV1 {
    pub nm_per_pixel: f64,
    #[serde(default)]
    pub pixel_scale_calibrated: bool,
    pub sensor_width: u16,
    pub sensor_height: u16,
    pub acq_time_ms: u64,
    pub event_store_budget_mib: u64,
    pub preview_interval_ms: u64,
    pub point_cloud_interval_ms: u64,
    pub disk_writer_buffer_mib: u64,
    #[serde(default)]
    pub record_sensor_telemetry: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CameraConfigurationProvenanceV1 {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_name: Option<String>,
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_revision: Option<u64>,
    pub sha256: String,
}

/// Programmed bias codes together with the per-unit factory trim the
/// configured offsets are relative to. `current - factory_default` is the
/// offset shown in the host settings panel.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SensorBiasReadbackV1 {
    pub current: SensorBiasCodesV1,
    pub factory_default: SensorBiasCodesV1,
}

/// Absolute, sensor-measured counterparts to the abstract camera settings.
///
/// The host camera configuration expresses biases as relative offsets around a
/// per-unit factory trim, so a plugin cannot derive physical values from it.
/// This is the sensor's own report.
///
/// **Availability:** present only while the host streams from a camera with a
/// monitoring block (currently the EVK4/IMX636). Replay, decoded imports and
/// deterministic offline analysis runs never carry it, because there is no
/// device to ask — a plugin that needs these values must treat them as
/// optional and must not change its results depending on their presence, or
/// live and offline runs over the same data will disagree.
///
/// A `None` field means the sensor cannot report that quantity. Only `refr`
/// has a physical unit among the biases; the other four are exposed as their
/// absolute code in `bias_codes`, because no vendor-documented conversion to
/// a physical unit exists.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct SensorMonitoringV1 {
    /// Measured pixel dead time (refractory period) in microseconds.
    #[serde(default)]
    pub pixel_dead_time_us: Option<f32>,
    /// Scene illumination in lux, integrated by the sensor.
    #[serde(default)]
    pub illumination_lux: Option<f32>,
    /// Sensor die temperature in degrees Celsius.
    #[serde(default)]
    pub temperature_c: Option<f32>,
    /// Absolute bias codes plus the factory defaults they offset from.
    #[serde(default)]
    pub bias_codes: Option<SensorBiasReadbackV1>,
    /// Seconds since the host actually read these values from the sensor. The
    /// host polls at a few hertz, so a reading is normally well under a second
    /// old but is never simultaneous with the frame it arrives on.
    #[serde(default)]
    pub age_s: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostViewRegistry {
    pub datasets: Vec<HostDatasetDescriptor>,
    pub views: Vec<HostViewDescriptor>,
    /// Row/dataset/cluster-scoped actions plugins offer to the host. The host
    /// renders buttons + modals from `param_schema` and publishes selections
    /// to [`CTX_INVESTIGATION_ACTION_REQUESTS`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<HostActionDescriptor>,
}

/// Declarative descriptor for a plugin-owned action the host should offer
/// alongside a dataset or a selected row/cluster.
///
/// The host renders a button whose enabled state is driven by `scope`, and
/// — if `param_schema` is set — a modal generated from the schema. Apply
/// publishes a [`HostActionRequest`] to [`CTX_INVESTIGATION_ACTION_REQUESTS`];
/// the plugin consumes it on its next frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostActionDescriptor {
    pub id: String,
    pub title: String,
    pub scope: HostActionScope,
    /// Optional JSON-encoded [`SettingsSchema`](crate::SettingsSchema) used
    /// to drive the action modal. When absent, the host renders a confirm
    /// dialog with no parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_schema: Option<serde_json::Value>,
}

/// When an action applies.
///
/// - `Dataset` — always applicable once the dataset exists.
/// - `Row` — applicable when exactly one row of the named dataset is selected.
/// - `Cluster` — applicable when selected rows share a common `group_column`
///   value on the named dataset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostActionScope {
    Dataset {
        dataset_id: String,
    },
    Row {
        dataset_id: String,
    },
    Cluster {
        dataset_id: String,
        group_column: String,
    },
}

/// A host-issued action invocation the plugin should consume on its next
/// frame. Plugins dedupe on `request_id` — the host never reuses an id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostActionRequest {
    /// Monotonically increasing id assigned by the host. Plugins treat the
    /// queue as read-only and skip any id they have already consumed.
    pub request_id: u64,
    pub action_id: String,
    pub scope_payload: HostActionScopePayload,
    /// Parameters captured from the action modal, keyed by `SettingItem.key`.
    /// Empty object when the descriptor has no `param_schema`.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Concrete scope the user clicked on when invoking the action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostActionScopePayload {
    None,
    Dataset {
        dataset_id: String,
    },
    Row {
        dataset_id: String,
        row_id: String,
    },
    Cluster {
        dataset_id: String,
        group_column: String,
        group_value: String,
    },
}

/// Queue payload published at [`CTX_INVESTIGATION_ACTION_REQUESTS`]. The
/// host owns the queue; plugins read and skip already-processed ids.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostActionRequestQueue {
    #[serde(default)]
    pub requests: Vec<HostActionRequest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostDatasetDescriptor {
    pub id: String,
    pub title: String,
    pub kind: HostDatasetKind,
    pub empty_message: String,
    pub display: Option<HostDatasetDisplayMetadata>,
    /// Declarative relations that let the host resolve row-to-row links
    /// across datasets without hand-wired joins (e.g., a localization's
    /// `cluster_id` → the matching candidate-event row).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<HostDatasetRelation>,
}

/// Declares that rows of *this* dataset can be linked to rows of
/// `target_dataset_id` by matching `via_column` (on this dataset) against
/// `target_column` (on the target).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostDatasetRelation {
    pub target_dataset_id: String,
    pub via_column: String,
    pub target_column: String,
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostViewPlacement {
    #[default]
    AnalysisPanel,
    Window,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostViewKind {
    #[default]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableSchema {
    pub columns: Vec<TableColumn>,
    pub coordinate_space_2d: Option<TableCoordinateSpace2d>,
    pub coordinate_space_3d: Option<TableCoordinateSpace3d>,
    pub row_id_column: Option<String>,
    pub time_column: Option<String>,
    pub layer_id: Option<String>,
    pub semantic_label: Option<String>,
    /// Declarative provenance used by the host to derive a per-row anchor
    /// timestamp and visibility span from column values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<TableRowProvenance>,
    /// Per-column display hints keyed by column `id`. Additive — unknown
    /// entries are ignored. Exports always operate on raw column values,
    /// never on the formatted display strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub column_display: Vec<TableColumnDisplayEntry>,
}

impl TableSchema {
    pub fn column(&self, id: &str) -> Option<&TableColumn> {
        self.columns.iter().find(|column| column.id == id)
    }

    /// Returns the display metadata registered for the given column id, if any.
    pub fn column_display(&self, column_id: &str) -> Option<&TableColumnDisplayMetadata> {
        self.column_display
            .iter()
            .find_map(|entry| (entry.column_id == column_id).then_some(&entry.display))
    }

    /// Resolve a single u64 value from a column for the given row.
    pub fn column_u64_at(&self, dataset: &TableDatasetV1, row: usize, name: &str) -> Option<u64> {
        let column = dataset.column(name)?;
        match &column.values {
            TableColumnValues::U64(values) => values.get(row).copied(),
            other => other
                .numeric_value(row)
                .map(|v| v.round().max(0.0).min(u64::MAX as f64) as u64),
        }
    }

    /// Resolve an anchor timestamp for a row using the declared provenance.
    /// Fallback order: `anchor_time_column` → midpoint of span → `time_column`.
    pub fn row_anchor_timestamp_us(&self, dataset: &TableDatasetV1, row: usize) -> Option<u64> {
        let provenance = self.provenance.as_ref();
        if let Some(column) = provenance.and_then(|p| p.anchor_time_column.as_deref()) {
            if let Some(ts) = self.column_u64_at(dataset, row, column) {
                return Some(ts);
            }
        }
        if let (Some(start_col), Some(end_col)) = (
            provenance.and_then(|p| p.span_start_column.as_deref()),
            provenance.and_then(|p| p.span_end_column.as_deref()),
        ) {
            if let (Some(start), Some(end)) = (
                self.column_u64_at(dataset, row, start_col),
                self.column_u64_at(dataset, row, end_col),
            ) {
                return Some(start + (end.saturating_sub(start)) / 2);
            }
        }
        if let Some(column) = self.time_column.as_deref() {
            return self.column_u64_at(dataset, row, column);
        }
        None
    }

    /// Span of a row `(start_us, end_us)` if declared; else `(anchor, anchor)`
    /// if the anchor resolves; else `None`. Both bounds inclusive.
    pub fn row_span_us(&self, dataset: &TableDatasetV1, row: usize) -> Option<(u64, u64)> {
        let provenance = self.provenance.as_ref();
        if let (Some(start_col), Some(end_col)) = (
            provenance.and_then(|p| p.span_start_column.as_deref()),
            provenance.and_then(|p| p.span_end_column.as_deref()),
        ) {
            if let (Some(start), Some(end)) = (
                self.column_u64_at(dataset, row, start_col),
                self.column_u64_at(dataset, row, end_col),
            ) {
                return Some((start, end));
            }
        }
        let anchor = self.row_anchor_timestamp_us(dataset, row)?;
        Some((anchor, anchor))
    }
}

/// Provenance hints that let the host derive anchor timestamps and spans
/// from row data without hard-coded column conventions. All columns are
/// referenced by id and are optional; the host falls back to
/// `TableSchema::time_column` when provenance is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableRowProvenance {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_time_column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_start_column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_end_column: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_frame_column: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableColumnDisplayEntry {
    pub column_id: String,
    pub display: TableColumnDisplayMetadata,
}

/// UI-only display hints. Formatting never leaks into exports.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableColumnDisplayMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<TableColumnDisplayFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_priority: Option<TableColumnWidthPriority>,
    #[serde(default)]
    pub hide_in_compact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Render this column as the summary card's headline field when a row
    /// is selected. At most one headline column is expected per schema.
    #[serde(default)]
    pub headline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TableColumnDisplayFormat {
    /// u64/i64/f64 interpreted as µs. UI renders relative `mm:ss.uuu` (or
    /// absolute when the replay origin is unknown); raw value on hover and
    /// in CSV.
    TimestampMicros,
    /// f64 with a fixed number of decimal digits in the UI.
    FixedPrecision { digits: u8 },
    /// Opaque identifier — no thousands separators, monospace in UI.
    Identifier,
    /// Short category label — hoverable full value when truncated.
    Category,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableColumnWidthPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TableColumn {
    pub id: String,
    pub title: String,
    pub value_type: TableValueType,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TableValueType {
    U64,
    I64,
    #[default]
    F64,
    String,
    Bool,
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
        if !raw.x_min.is_finite() || !raw.x_max.is_finite() || raw.x_min > raw.x_max {
            return Err(D::Error::custom(
                "x_min and x_max must be finite and x_min <= x_max",
            ));
        }
        if !raw.y_min.is_finite() || !raw.y_max.is_finite() || raw.y_min > raw.y_max {
            return Err(D::Error::custom(
                "y_min and y_max must be finite and y_min <= y_max",
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
        if !raw.x_min.is_finite() || !raw.x_max.is_finite() || raw.x_min > raw.x_max {
            return Err(D::Error::custom(
                "x_min and x_max must be finite and x_min <= x_max",
            ));
        }
        if !raw.y_min.is_finite() || !raw.y_max.is_finite() || raw.y_min > raw.y_max {
            return Err(D::Error::custom(
                "y_min and y_max must be finite and y_min <= y_max",
            ));
        }
        if !raw.z_min.is_finite() || !raw.z_max.is_finite() || raw.z_min > raw.z_max {
            return Err(D::Error::custom(
                "z_min and z_max must be finite and z_min <= z_max",
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostMarkerShape {
    #[default]
    Circle,
    Square,
    Diamond,
    Cross,
    Point,
    Box,
    Ellipse,
    FilledCircle,
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

        let column_map: std::collections::HashMap<&str, &TableColumnData> = self
            .columns
            .iter()
            .map(|c| (c.column_id.as_str(), c))
            .collect();

        for schema_column in &schema.columns {
            let Some(column) = column_map.get(schema_column.id.as_str()) else {
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

        let schema_ids: std::collections::HashSet<&str> =
            schema.columns.iter().map(|c| c.id.as_str()).collect();
        for column in &self.columns {
            if !schema_ids.contains(column.column_id.as_str()) {
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

#[cfg(test)]
mod camera_command_tests {
    use super::*;

    fn snapshot() -> CameraConfigurationSnapshotV1 {
        CameraConfigurationSnapshotV1 {
            schema_version: 1,
            biases: CameraBiasOffsetsV1 {
                diff_on: 12,
                diff_off: -7,
                fo: 0,
                hpf: 0,
                refr: 0,
            },
            roi: RoiV1 {
                x: 1,
                y: 2,
                width: 640,
                height: 480,
            },
            masked_pixels: vec![(3, 4)],
            digital_filter: CameraDigitalFilterV1 {
                stc_enabled: false,
                stc_threshold_us: 0,
                trail_enabled: false,
                erc_enabled: Some(false),
            },
            external_trigger: CameraExternalTriggerV1::default(),
            global: CameraGlobalSettingsV1 {
                nm_per_pixel: 1_000.0,
                pixel_scale_calibrated: true,
                sensor_width: 1280,
                sensor_height: 720,
                acq_time_ms: 1,
                event_store_budget_mib: 512,
                preview_interval_ms: 16,
                point_cloud_interval_ms: 50,
                disk_writer_buffer_mib: 64,
                record_sensor_telemetry: true,
            },
        }
    }

    #[test]
    fn camera_configuration_command_roundtrips_without_losing_sensor_recording() {
        let command = HostCommand::ApplyCameraConfiguration {
            configuration: CameraConfigurationSourceV1::Snapshot {
                snapshot: snapshot(),
            },
        };
        let json = serde_json::to_string(&command).expect("serialize");
        let decoded: HostCommand = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded, command);
        assert!(json.contains("record_sensor_telemetry"));
    }

    #[test]
    fn current_configuration_source_is_explicit() {
        let command = HostCommand::ApplyCameraConfiguration {
            configuration: CameraConfigurationSourceV1::Current,
        };
        let json = serde_json::to_string(&command).expect("serialize");
        assert_eq!(
            json,
            r#"{"command":"apply_camera_configuration","configuration":{"source":"current"}}"#
        );
    }
}
