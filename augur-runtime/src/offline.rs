use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use augur_core::{
    decoded_replay::{DecodedEventFileCamera, DecodedReplayEventSource},
    pipeline::accumulate_compact_frame,
    replay::{RawFileCamera, RawReplayEventSource, ReplayFileInfo},
};
use augur_event_types::{CompactEvent, EventSource, FetchError};
use augur_plugin_api::{
    GlobalSettings, HostDatasetKind, Image2dV1, PluginInput, Series1dV1, TableColumnValues,
    TableDatasetV1, TableSchema, CTX_GLOBAL_SETTINGS,
};
use image::{ImageFormat, RgbaImage};
use serde::Deserialize;

use crate::{
    collect_live_host_snapshots, AnalysisPassContext, EventHistoryMaterializationCache,
    HostSnapshotCache, LivePluginHostSnapshot, PluginEventHistory, PluginManager,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampWindow {
    pub start_us: u64,
    pub end_us: u64,
}

#[derive(Debug, Clone)]
pub struct TimestampWindower {
    next_start_us: u64,
    acq_time_us: u64,
    last_end_exclusive_us: u64,
    finished: bool,
}

impl TimestampWindower {
    pub fn new(t_start_us: u64, acq_time_us: u64, last_event_ts_us: u64) -> Self {
        Self {
            next_start_us: t_start_us,
            acq_time_us: acq_time_us.max(1),
            last_end_exclusive_us: last_event_ts_us.saturating_add(1),
            finished: false,
        }
    }
}

impl Iterator for TimestampWindower {
    type Item = TimestampWindow;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.next_start_us >= self.last_end_exclusive_us {
            return None;
        }
        let start_us = self.next_start_us;
        let regular_end = start_us.saturating_add(self.acq_time_us);
        let end_us = regular_end.min(self.last_end_exclusive_us);
        self.next_start_us = regular_end;
        if end_us >= self.last_end_exclusive_us {
            self.finished = true;
        }
        Some(TimestampWindow { start_us, end_us })
    }
}

#[derive(Debug, Clone, Default)]
pub struct OfflineAnalysisConfig {
    pub t_start_us: Option<u64>,
    /// Exclusive end of the analyzed range: windows cover `[t_start, t_end)`.
    pub t_end_us: Option<u64>,
    pub acq_time_us: Option<u64>,
    pub acq_time_ms: Option<u64>,
    pub plugins: BTreeMap<String, OfflinePluginConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OfflinePluginConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub settings: BTreeMap<String, toml::Value>,
    #[serde(flatten)]
    pub inline_settings: BTreeMap<String, toml::Value>,
}

impl OfflineAnalysisConfig {
    pub fn from_toml_file(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("reading {} failed: {err}", path.display()))?;
        toml::from_str(&text).map_err(|err| {
            format!(
                "offline analysis config {} is invalid: {err}",
                path.display()
            )
        })
    }

    fn acq_time_us_or_default(&self, default_ms: u64) -> u64 {
        self.acq_time_us
            .or_else(|| self.acq_time_ms.map(|ms| ms.saturating_mul(1_000)))
            .unwrap_or_else(|| default_ms.max(1).saturating_mul(1_000))
            .max(1)
    }
}

impl<'de> Deserialize<'de> for OfflineAnalysisConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawConfig {
            #[serde(default)]
            t_start_us: Option<u64>,
            #[serde(default)]
            t_end_us: Option<u64>,
            #[serde(default)]
            acq_time_us: Option<u64>,
            #[serde(default)]
            acq_time_ms: Option<u64>,
            #[serde(default)]
            plugins: BTreeMap<String, OfflinePluginConfig>,
        }

        let raw = RawConfig::deserialize(deserializer)?;
        Ok(Self {
            t_start_us: raw.t_start_us,
            t_end_us: raw.t_end_us,
            acq_time_us: raw.acq_time_us,
            acq_time_ms: raw.acq_time_ms,
            plugins: raw.plugins,
        })
    }
}

#[derive(Debug, Clone)]
pub struct OfflineAnalysisOptions {
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    pub plugins_dir: Option<PathBuf>,
    pub config: OfflineAnalysisConfig,
    pub stop: Option<Arc<AtomicBool>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfflineProgress {
    pub processed_windows: u64,
    pub total_windows: u64,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

#[derive(Debug, Clone)]
pub struct OfflineAnalysisSummary {
    pub processed_windows: u64,
    pub exported_files: Vec<PathBuf>,
    pub host_snapshots: Vec<LivePluginHostSnapshot>,
}

enum OfflineInput {
    Decoded {
        source: DecodedReplayEventSource,
        info: ReplayFileInfo,
        first_event_ts_us: u64,
        last_event_ts_us: u64,
    },
    Raw {
        source: Box<RawReplayEventSource>,
        info: ReplayFileInfo,
    },
}

impl OfflineInput {
    fn open(path: &Path) -> Result<Self, String> {
        match path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref()
        {
            Some("raw") => {
                let (_camera, _controls, info) = RawFileCamera::open(path)
                    .map_err(|err| format!("opening raw replay failed: {err}"))?;
                Ok(Self::Raw {
                    source: Box::new(RawReplayEventSource::new(path, info.clone())),
                    info,
                })
            }
            _ => {
                let (_camera, _controls, info, events) = DecodedEventFileCamera::open(path)
                    .map_err(|err| format!("opening decoded replay failed: {err}"))?;
                let first_event_ts_us = events
                    .first()
                    .map(|event| event.timestamp)
                    .ok_or_else(|| "decoded replay contains no events".to_owned())?;
                let last_event_ts_us = events
                    .last()
                    .map(|event| event.timestamp)
                    .unwrap_or(first_event_ts_us);
                Ok(Self::Decoded {
                    source: DecodedReplayEventSource::new(events),
                    info,
                    first_event_ts_us,
                    last_event_ts_us,
                })
            }
        }
    }

    fn info(&self) -> &ReplayFileInfo {
        match self {
            Self::Decoded { info, .. } | Self::Raw { info, .. } => info,
        }
    }

    fn first_event_ts_us(&self) -> u64 {
        match self {
            Self::Decoded {
                first_event_ts_us, ..
            } => *first_event_ts_us,
            Self::Raw { info, .. } => info.first_timestamp_us,
        }
    }

    fn last_event_ts_us(&self) -> u64 {
        match self {
            Self::Decoded {
                last_event_ts_us, ..
            } => *last_event_ts_us,
            Self::Raw { info, .. } => info
                .first_timestamp_us
                .saturating_add(info.total_duration_us),
        }
    }

    fn fetch_half_open(&self, start_us: u64, end_us: u64) -> Result<Vec<CompactEvent>, String> {
        if start_us >= end_us {
            return Ok(Vec::new());
        }
        let inclusive_end = end_us.saturating_sub(1);
        let chunk = match self {
            Self::Decoded { source, .. } => source.fetch_range(start_us, inclusive_end),
            Self::Raw { source, .. } => source.fetch_range(start_us, inclusive_end),
        };
        match chunk {
            Ok(chunk) => Ok(chunk
                .events
                .into_iter()
                .filter(|event| {
                    let ts = event.timestamp_us();
                    ts >= start_us && ts < end_us
                })
                .collect()),
            Err(FetchError::OutOfTimeline) => Ok(Vec::new()),
            Err(err) => Err(format!("fetching [{start_us}, {end_us}) failed: {err}")),
        }
    }
}

/// Timeline metadata for a replay file, used to offer range selection
/// before an analysis run without keeping the file open.
#[derive(Debug, Clone)]
pub struct ReplayFileProbe {
    pub info: ReplayFileInfo,
    pub first_event_ts_us: u64,
    pub last_event_ts_us: u64,
}

pub fn probe_replay_file(path: &Path) -> Result<ReplayFileProbe, String> {
    let input = OfflineInput::open(path)?;
    Ok(ReplayFileProbe {
        first_event_ts_us: input.first_event_ts_us(),
        last_event_ts_us: input.last_event_ts_us(),
        info: input.info().clone(),
    })
}

pub fn run_offline_analysis(
    options: OfflineAnalysisOptions,
    mut progress: impl FnMut(OfflineProgress),
) -> Result<OfflineAnalysisSummary, String> {
    let input = OfflineInput::open(&options.input_path)?;
    let info = input.info().clone();
    let acq_time_us = options.config.acq_time_us_or_default(1);
    let t_start_us = options
        .config
        .t_start_us
        .unwrap_or_else(|| input.first_event_ts_us());
    // Clamp the analyzed range to a configured exclusive end: the last
    // window may not reach past `t_end_us`, matching half-open semantics.
    let last_event_ts_us = match options.config.t_end_us {
        Some(t_end_us) => {
            if t_end_us <= t_start_us {
                return Err(format!(
                    "analysis range [{t_start_us}, {t_end_us}) us is empty; \
                     t_end_us must be greater than t_start_us"
                ));
            }
            input.last_event_ts_us().min(t_end_us - 1)
        }
        None => input.last_event_ts_us(),
    };
    let total_windows =
        TimestampWindower::new(t_start_us, acq_time_us, last_event_ts_us).count() as u64;

    let mut plugin_manager = match options.plugins_dir {
        Some(path) => PluginManager::new(path),
        None => PluginManager::new_default(),
    };
    plugin_manager.scan_and_load()?;
    apply_offline_plugin_config(&mut plugin_manager, &options.config)?;
    for record in plugin_manager.records_mut() {
        if let Some(plugin) = record.plugin_mut() {
            plugin.reset();
        }
    }

    let retained_history_needed = plugin_manager.records().iter().any(|record| {
        record
            .plugin()
            .is_some_and(|plugin| plugin.enabled() && plugin.capabilities().retained_event_history)
    });

    let mut event_store = PluginEventHistory::default();
    let mut context_data = HashMap::<String, Vec<u8>>::new();
    let mut persistent_context_data = HashMap::<String, Vec<u8>>::new();
    let mut processed_windows = 0_u64;

    for window in TimestampWindower::new(t_start_us, acq_time_us, last_event_ts_us) {
        if options
            .stop
            .as_ref()
            .is_some_and(|stop| stop.load(Ordering::Relaxed))
        {
            return Err("offline analysis cancelled".into());
        }

        let events = input.fetch_half_open(window.start_us, window.end_us)?;
        let frame = accumulate_compact_frame(
            &events,
            info.width,
            info.height,
            window.start_us,
            window.end_us,
            retained_history_needed,
        );
        let raw_events = events;
        if retained_history_needed {
            event_store.push_frame(&frame);
        } else {
            event_store.clear();
        }

        context_data.clear();
        publish_global_settings(
            &mut context_data,
            GlobalSettings {
                nm_per_pixel: info.metadata.pixel_pitch_nm.unwrap_or(1.0),
                sensor_width: info.width,
                sensor_height: info.height,
                acq_time_ms: acq_time_us.div_ceil(1_000),
                event_store_budget_bytes: event_store.memory_budget_bytes(),
            },
        )?;

        let mut analysis_output = augur_core::analysis::AnalysisOutput::default();
        let history_cache = EventHistoryMaterializationCache::default();
        let pass = AnalysisPassContext {
            event_store: &event_store,
            history_cache: &history_cache,
        };
        for phase in [
            PluginInput::FrameOnly,
            PluginInput::RawEvents,
            PluginInput::DerivedData,
        ] {
            for record in plugin_manager.records_mut() {
                let Some(plugin) = record.plugin_mut() else {
                    continue;
                };
                if plugin.enabled() && plugin.input_kind() == phase {
                    plugin.process_frame(
                        &frame,
                        if phase == PluginInput::RawEvents {
                            &raw_events
                        } else {
                            &[]
                        },
                        &pass,
                        &mut analysis_output,
                        &mut context_data,
                        &mut persistent_context_data,
                    );
                }
            }
        }

        processed_windows = processed_windows.saturating_add(1);
        progress(OfflineProgress {
            processed_windows,
            total_windows,
            window_start_us: window.start_us,
            window_end_us: window.end_us,
        });
    }

    let host_snapshots =
        collect_live_host_snapshots(&plugin_manager, &mut HostSnapshotCache::default());
    export_outputs(
        &mut plugin_manager,
        &options.output_dir,
        options.stop.as_ref(),
    )
    .map(|exported_files| OfflineAnalysisSummary {
        processed_windows,
        exported_files,
        host_snapshots,
    })
}

fn apply_offline_plugin_config(
    plugin_manager: &mut PluginManager,
    config: &OfflineAnalysisConfig,
) -> Result<(), String> {
    for record in plugin_manager.records_mut() {
        let Some(plugin) = record.plugin_mut() else {
            continue;
        };
        let Some(plugin_config) = config.plugins.get(plugin.name()) else {
            continue;
        };
        if let Some(enabled) = plugin_config.enabled {
            plugin.set_enabled(enabled);
        }
        for (key, value) in plugin_config.setting_values() {
            plugin.set_setting_value(&key, &toml_value_to_json(value)?)?;
        }
    }
    Ok(())
}

impl OfflinePluginConfig {
    fn setting_values(&self) -> BTreeMap<String, toml::Value> {
        let mut values = self.inline_settings.clone();
        values.extend(self.settings.clone());
        values
    }
}

fn publish_global_settings(
    context_data: &mut HashMap<String, Vec<u8>>,
    global_settings: GlobalSettings,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(&global_settings)
        .map_err(|err| format!("serializing global settings failed: {err}"))?;
    context_data.insert(CTX_GLOBAL_SETTINGS.to_owned(), bytes);
    Ok(())
}

fn export_outputs(
    plugin_manager: &mut PluginManager,
    output_dir: &Path,
    stop: Option<&Arc<AtomicBool>>,
) -> Result<Vec<PathBuf>, String> {
    if output_dir.exists() {
        return Err(format!(
            "output directory {} already exists; choose a new directory",
            output_dir.display()
        ));
    }
    let parent = output_dir.parent().unwrap_or_else(|| Path::new("."));
    let stem = output_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("analysis-output");
    let tmp_dir = parent.join(format!("{stem}.tmp-{}", std::process::id()));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)
            .map_err(|err| format!("removing stale {} failed: {err}", tmp_dir.display()))?;
    }
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| format!("creating {} failed: {err}", tmp_dir.display()))?;

    let export_result = export_outputs_to_tmp(plugin_manager, &tmp_dir, stop);
    match export_result {
        Ok(exported_files) => {
            fs::rename(&tmp_dir, output_dir).map_err(|err| {
                format!(
                    "moving {} to {} failed: {err}",
                    tmp_dir.display(),
                    output_dir.display()
                )
            })?;
            Ok(exported_files
                .into_iter()
                .map(|path| output_dir.join(path.file_name().unwrap_or_default()))
                .collect())
        }
        Err(err) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            Err(err)
        }
    }
}

fn export_outputs_to_tmp(
    plugin_manager: &mut PluginManager,
    output_dir: &Path,
    stop: Option<&Arc<AtomicBool>>,
) -> Result<Vec<PathBuf>, String> {
    let mut exported = Vec::new();
    for record in plugin_manager.records_mut() {
        let Some(plugin) = record.plugin_mut() else {
            continue;
        };
        if !plugin.enabled() {
            continue;
        }
        let registry = plugin.host_views()?;
        for descriptor in registry.datasets {
            if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
                return Err("offline analysis cancelled".into());
            }
            let Some(bytes) = plugin.host_view_dataset(&descriptor.id)? else {
                continue;
            };
            let plugin_slug = slugify(plugin.name());
            let dataset_slug = slugify(&descriptor.id);
            let path = match &descriptor.kind {
                HostDatasetKind::TableV1(schema) => {
                    let dataset: TableDatasetV1 =
                        serde_json::from_slice(&bytes).map_err(|err| {
                            format!("table dataset {} JSON is invalid: {err}", descriptor.id)
                        })?;
                    dataset.validate_against_schema(schema)?;
                    let path = output_dir.join(format!("{plugin_slug}__{dataset_slug}.csv"));
                    export_table_csv_to_path(&path, schema, &dataset)?;
                    path
                }
                HostDatasetKind::Image2dV1 => {
                    let dataset: Image2dV1 = serde_json::from_slice(&bytes).map_err(|err| {
                        format!("image dataset {} JSON is invalid: {err}", descriptor.id)
                    })?;
                    let path = output_dir.join(format!("{plugin_slug}__{dataset_slug}.png"));
                    export_image_to_path(&path, &dataset)?;
                    path
                }
                HostDatasetKind::Series1dV1 => {
                    let dataset: Series1dV1 = serde_json::from_slice(&bytes).map_err(|err| {
                        format!("series dataset {} JSON is invalid: {err}", descriptor.id)
                    })?;
                    let path = output_dir.join(format!("{plugin_slug}__{dataset_slug}.json"));
                    let file = File::create(&path)
                        .map_err(|err| format!("creating {} failed: {err}", path.display()))?;
                    serde_json::to_writer_pretty(file, &dataset)
                        .map_err(|err| format!("writing {} failed: {err}", path.display()))?;
                    path
                }
            };
            exported.push(path);
        }
    }
    Ok(exported)
}

pub fn export_table_csv_to_path(
    path: &Path,
    schema: &TableSchema,
    dataset: &TableDatasetV1,
) -> Result<(), String> {
    let file =
        File::create(path).map_err(|err| format!("creating {} failed: {err}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for (index, column) in schema.columns.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",").map_err(|err| err.to_string())?;
        }
        write_csv_cell(&mut writer, &column.title).map_err(|err| err.to_string())?;
    }
    writer.write_all(b"\n").map_err(|err| err.to_string())?;

    for row in 0..dataset.row_count() {
        for (index, column) in schema.columns.iter().enumerate() {
            if index > 0 {
                writer.write_all(b",").map_err(|err| err.to_string())?;
            }
            let value = dataset
                .column(&column.id)
                .and_then(|column| table_cell_value(&column.values, row))
                .unwrap_or_default();
            write_csv_cell(&mut writer, &value).map_err(|err| err.to_string())?;
        }
        writer.write_all(b"\n").map_err(|err| err.to_string())?;
    }
    writer
        .flush()
        .map_err(|err| format!("writing {} failed: {err}", path.display()))
}

pub fn export_image_to_path(path: &Path, image: &Image2dV1) -> Result<(), String> {
    image.validate()?;
    let rgba = image_to_rgba(image)?;
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

fn image_to_rgba(image: &Image2dV1) -> Result<RgbaImage, String> {
    let finite_values: Vec<f32> = image
        .pixels
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    let min = finite_values
        .iter()
        .copied()
        .reduce(f32::min)
        .unwrap_or(0.0);
    let max = finite_values
        .iter()
        .copied()
        .reduce(f32::max)
        .unwrap_or(min);
    let scale = if max > min { 255.0 / (max - min) } else { 0.0 };
    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
    for value in &image.pixels {
        let gray = if value.is_finite() {
            ((*value - min) * scale).clamp(0.0, 255.0) as u8
        } else {
            0
        };
        bytes.extend_from_slice(&[gray, gray, gray, 255]);
    }
    RgbaImage::from_raw(image.width, image.height, bytes)
        .ok_or_else(|| "failed to build RGBA export image".to_owned())
}

fn table_cell_value(values: &TableColumnValues, row: usize) -> Option<String> {
    match values {
        TableColumnValues::U64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::I64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::F64(values) => values.get(row).map(ToString::to_string),
        TableColumnValues::String(values) => values.get(row).cloned(),
        TableColumnValues::Bool(values) => values.get(row).map(ToString::to_string),
    }
}

fn write_csv_cell(mut writer: impl Write, value: &str) -> std::io::Result<()> {
    let needs_quotes = value.contains([',', '"', '\n', '\r']);
    if !needs_quotes {
        return writer.write_all(value.as_bytes());
    }
    writer.write_all(b"\"")?;
    for (index, part) in value.split('"').enumerate() {
        if index > 0 {
            writer.write_all(b"\"\"")?;
        }
        writer.write_all(part.as_bytes())?;
    }
    writer.write_all(b"\"")
}

/// Inverse of `toml_value_to_json`, used by hosts that snapshot live JSON
/// plugin settings into an `OfflineAnalysisConfig`. JSON nulls have no TOML
/// representation and are skipped.
pub fn json_value_to_toml(value: &serde_json::Value) -> Option<toml::Value> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(value) => Some(toml::Value::Boolean(*value)),
        serde_json::Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                Some(toml::Value::Integer(value))
            } else {
                value.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::String(value) => Some(toml::Value::String(value.clone())),
        serde_json::Value::Array(values) => Some(toml::Value::Array(
            values.iter().filter_map(json_value_to_toml).collect(),
        )),
        serde_json::Value::Object(map) => {
            let mut table = toml::value::Table::new();
            for (key, value) in map {
                if let Some(value) = json_value_to_toml(value) {
                    table.insert(key.clone(), value);
                }
            }
            Some(toml::Value::Table(table))
        }
    }
}

fn toml_value_to_json(value: toml::Value) -> Result<serde_json::Value, String> {
    let json = match value {
        toml::Value::String(value) => serde_json::Value::String(value),
        toml::Value::Integer(value) => serde_json::Value::Number(value.into()),
        toml::Value::Float(value) => serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| "TOML float value is not finite".to_owned())?,
        toml::Value::Boolean(value) => serde_json::Value::Bool(value),
        toml::Value::Datetime(value) => serde_json::Value::String(value.to_string()),
        toml::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(toml_value_to_json)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        toml::Value::Table(values) => {
            let mut map = serde_json::Map::new();
            for (key, value) in values {
                map.insert(key, toml_value_to_json(value)?);
            }
            serde_json::Value::Object(map)
        }
    };
    Ok(json)
}

fn slugify(value: &str) -> String {
    let slug: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if slug.trim_matches('_').is_empty() {
        "output".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{atomic::AtomicBool, Arc},
        time::SystemTime,
    };

    #[test]
    fn windower_emits_half_open_partial_tail() {
        let windows: Vec<_> = TimestampWindower::new(10, 5, 21).collect();
        assert_eq!(
            windows,
            vec![
                TimestampWindow {
                    start_us: 10,
                    end_us: 15
                },
                TimestampWindow {
                    start_us: 15,
                    end_us: 20
                },
                TimestampWindow {
                    start_us: 20,
                    end_us: 22
                }
            ]
        );
    }

    #[test]
    fn offline_analysis_clamps_windows_to_configured_end() {
        let root = unique_temp_dir("offline-range");
        fs::create_dir_all(&root).expect("temp root");
        let input = root.join("events.csv");
        fs::write(
            &input,
            "%geometry:8,8\n1,1,1,10\n2,1,0,11\n3,1,1,15\n4,2,1,25\n",
        )
        .expect("write csv replay");
        let plugins_dir = root.join("plugins");
        fs::create_dir_all(&plugins_dir).expect("plugins dir");

        let mut windows = Vec::new();
        let summary = run_offline_analysis(
            OfflineAnalysisOptions {
                input_path: input,
                output_dir: root.join("out"),
                plugins_dir: Some(plugins_dir),
                config: OfflineAnalysisConfig {
                    t_start_us: Some(10),
                    t_end_us: Some(20),
                    acq_time_us: Some(5),
                    ..OfflineAnalysisConfig::default()
                },
                stop: None,
            },
            |progress| windows.push((progress.window_start_us, progress.window_end_us)),
        )
        .expect("offline run");

        assert_eq!(summary.processed_windows, 2);
        assert_eq!(windows, vec![(10, 15), (15, 20)]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn offline_analysis_rejects_empty_range() {
        let root = unique_temp_dir("offline-empty-range");
        fs::create_dir_all(&root).expect("temp root");
        let input = root.join("events.csv");
        fs::write(&input, "%geometry:8,8\n1,1,1,10\n").expect("write csv replay");
        let plugins_dir = root.join("plugins");
        fs::create_dir_all(&plugins_dir).expect("plugins dir");

        let err = run_offline_analysis(
            OfflineAnalysisOptions {
                input_path: input,
                output_dir: root.join("out"),
                plugins_dir: Some(plugins_dir),
                config: OfflineAnalysisConfig {
                    t_start_us: Some(20),
                    t_end_us: Some(20),
                    ..OfflineAnalysisConfig::default()
                },
                stop: None,
            },
            |_| {},
        )
        .expect_err("empty range must be rejected");
        assert!(err.contains("range"), "unexpected error: {err}");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn probe_reports_event_timeline_bounds() {
        let root = unique_temp_dir("offline-probe");
        fs::create_dir_all(&root).expect("temp root");
        let input = root.join("events.csv");
        fs::write(&input, "%geometry:8,8\n1,1,1,10\n2,1,0,11\n3,1,1,15\n")
            .expect("write csv replay");

        let probe = probe_replay_file(&input).expect("probe");
        assert_eq!(probe.first_event_ts_us, 10);
        assert_eq!(probe.last_event_ts_us, 15);
        assert_eq!(probe.info.width, 8);
        assert_eq!(probe.info.height, 8);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn toml_plugin_config_supports_inline_and_nested_settings() {
        let config: OfflineAnalysisConfig = toml::from_str(
            r#"
            acq_time_ms = 2
            t_end_us = 42

            [plugins.demo]
            enabled = true
            threshold = 1.5

            [plugins.demo.settings]
            mode = "fast"
            "#,
        )
        .expect("config");
        let settings = config.plugins["demo"].setting_values();
        assert_eq!(config.acq_time_us_or_default(1), 2_000);
        assert_eq!(config.t_end_us, Some(42));
        assert_eq!(settings["threshold"].as_float(), Some(1.5));
        assert_eq!(settings["mode"].as_str(), Some("fast"));
    }

    #[test]
    fn offline_analysis_repeated_runs_are_byte_identical_without_plugins() {
        let root = unique_temp_dir("offline-determinism");
        fs::create_dir_all(&root).expect("temp root");
        let input = root.join("events.csv");
        fs::write(&input, "%geometry:8,8\n1,1,1,10\n2,1,0,11\n3,1,1,15\n")
            .expect("write csv replay");

        let plugins_dir = root.join("plugins");
        fs::create_dir_all(&plugins_dir).expect("plugins dir");
        let first_out = root.join("out-a");
        let second_out = root.join("out-b");
        let config = OfflineAnalysisConfig {
            t_start_us: Some(10),
            acq_time_us: Some(3),
            ..OfflineAnalysisConfig::default()
        };

        let first = run_offline_analysis(
            OfflineAnalysisOptions {
                input_path: input.clone(),
                output_dir: first_out.clone(),
                plugins_dir: Some(plugins_dir.clone()),
                config: config.clone(),
                stop: Some(Arc::new(AtomicBool::new(false))),
            },
            |_| {},
        )
        .expect("first offline run");
        let second = run_offline_analysis(
            OfflineAnalysisOptions {
                input_path: input,
                output_dir: second_out.clone(),
                plugins_dir: Some(plugins_dir),
                config,
                stop: Some(Arc::new(AtomicBool::new(false))),
            },
            |_| {},
        )
        .expect("second offline run");

        assert_eq!(first.processed_windows, second.processed_windows);
        assert_eq!(dir_fingerprint(&first_out), dir_fingerprint(&second_out));
        let _ = fs::remove_dir_all(root);
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "augur-runtime-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    fn dir_fingerprint(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut entries = Vec::new();
        collect_dir_fingerprint(path, path, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    fn collect_dir_fingerprint(root: &Path, path: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(path).expect("read output dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                collect_dir_fingerprint(root, &path, entries);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .expect("entry should live under root")
                    .to_string_lossy()
                    .into_owned();
                entries.push((relative, fs::read(path).expect("read output file")));
            }
        }
    }
}
