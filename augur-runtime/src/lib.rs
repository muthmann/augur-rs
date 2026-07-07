use std::{
    cell::UnsafeCell,
    collections::{HashMap, VecDeque},
    ffi::c_void,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod offline;

pub use offline::{
    json_value_to_toml, probe_replay_file, run_offline_analysis, OfflineAnalysisConfig,
    OfflineAnalysisOptions, OfflineAnalysisSummary, OfflinePluginConfig, OfflineProgress,
    ReplayFileProbe, TimestampWindow, TimestampWindower,
};

use augur_core::{
    analysis::{
        AnalysisOutput, AnalysisSeverity as CoreSeverity, AnalysisWarning, MarkerOverlayItem,
        MarkerShape, Overlay, Pixel, SubpixelMarker,
    },
    pipeline::{LiveEventFrameBatch, LiveEventSource, PreviewFrame},
};
use augur_event_types::{BackpressureBehavior, CursorId, CursorPolicy};
use augur_plugin_api::{
    AnalysisSeverity, FfiCdEvent, FfiColorRgba, FfiEventFrame, FfiEventStoreHandle,
    FfiMarkerOverlayItem, FfiMarkerShape, FfiOutputCallbacks, FfiPixel, FfiPluginContext,
    FfiPreviewFrame, FfiSlice, FfiString, FfiSubpixelMarker, HostDatasetDescriptor,
    HostDatasetKind, HostViewRegistry, Image2dV1, PluginCapabilities, PluginDiscontinuity,
    PluginEntry, PluginInput, PluginStateKind, PluginVTable, Series1dV1, SettingsSchema,
    StatusEntry, TableDatasetV1, PLUGIN_ABI_VERSION, PLUGIN_ENTRY_SYMBOL,
};
use libloading::Library;
use serde::Deserialize;
use serde_json::Value;

pub const PLUGIN_UI_CACHE_INTERVAL: Duration = Duration::from_millis(250);
const MIN_PLAUSIBLE_FUNCTION_POINTER: usize = 4096;
const DEFAULT_EVENT_HISTORY_BUDGET_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LivePluginState {
    pub name: String,
    pub enabled: bool,
    pub settings: Vec<(String, Value)>,
}

#[derive(Debug, Clone, Default)]
pub struct LivePluginStateSnapshot {
    pub plugins: Vec<LivePluginState>,
}

/// A host-view dataset decoded once on the worker thread. Payloads are
/// `Arc`-shared so re-publishing an unchanged dataset with each result is a
/// refcount bump instead of a serialize/clone/parse round-trip on the GUI
/// thread.
#[derive(Debug, Clone, PartialEq)]
pub enum HostDatasetSnapshot {
    Table(Arc<TableDatasetV1>),
    Image2d(Arc<Image2dV1>),
    Series1d(Arc<Series1dV1>),
}

/// Decodes and validates a plugin-provided dataset payload against its
/// descriptor.
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
        HostDatasetKind::Image2dV1 => {
            let dataset: Image2dV1 = serde_json::from_slice(bytes)
                .map_err(|err| format!("image dataset JSON is invalid: {err}"))?;
            Ok(HostDatasetSnapshot::Image2d(Arc::new(dataset)))
        }
        HostDatasetKind::Series1dV1 => {
            let dataset: Series1dV1 = serde_json::from_slice(bytes)
                .map_err(|err| format!("series dataset JSON is invalid: {err}"))?;
            Ok(HostDatasetSnapshot::Series1d(Arc::new(dataset)))
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveHostDatasetSnapshot {
    pub id: String,
    pub generation: u64,
    /// Decoded dataset (`None` when the plugin returned no data); decode and
    /// schema-validation failures carry the error message.
    pub payload: Option<Result<HostDatasetSnapshot, String>>,
}

/// Worker-side cache that skips fetching, decoding, and re-shipping dataset
/// payloads whose provider generation is unchanged. Generation `0` means
/// "no counter" and is refreshed on every collection pass.
#[derive(Debug, Default)]
pub struct HostSnapshotCache {
    entries: HashMap<(usize, String), CachedHostSnapshot>,
}

#[derive(Debug, Clone)]
struct CachedHostSnapshot {
    generation: u64,
    payload: Option<Result<HostDatasetSnapshot, String>>,
}

impl HostSnapshotCache {
    /// Drops all cached payloads. Must be called whenever plugin instances
    /// are reconfigured, reloaded, or reset — a fresh instance may reuse
    /// generation numbers for different data.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns the payload for `key`, calling `fetch` only when the provider
    /// generation changed (generation `0` always refetches).
    fn resolve(
        &mut self,
        key: (usize, String),
        generation: u64,
        fetch: impl FnOnce() -> Option<Result<HostDatasetSnapshot, String>>,
    ) -> Option<Result<HostDatasetSnapshot, String>> {
        let reuse = generation != 0
            && self
                .entries
                .get(&key)
                .is_some_and(|cached| cached.generation == generation);
        if !reuse {
            let payload = fetch();
            self.entries.insert(
                key.clone(),
                CachedHostSnapshot {
                    generation,
                    payload,
                },
            );
        }
        self.entries[&key].payload.clone()
    }
}

#[derive(Debug, Clone)]
pub struct LivePluginHostSnapshot {
    pub index: usize,
    pub name: String,
    pub registry: Option<HostViewRegistry>,
    pub datasets: Vec<LiveHostDatasetSnapshot>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LiveAnalysisResult {
    pub epoch: u64,
    pub output: AnalysisOutput,
    pub context_data: HashMap<String, Vec<u8>>,
    pub persistent_data: HashMap<String, Vec<u8>>,
    pub host_snapshots: Vec<LivePluginHostSnapshot>,
    /// Highest host action request id that was visible on the persistent bus
    /// while this job ran. The GUI retires delivered requests up to this id.
    pub action_request_watermark: u64,
}

#[derive(Debug)]
pub struct LiveAnalysisWorker {
    tx: mpsc::Sender<LiveAnalysisCommand>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl LiveAnalysisWorker {
    pub fn spawn(
        plugins_dir: PathBuf,
        memory_budget_bytes: usize,
    ) -> (Self, mpsc::Receiver<LiveAnalysisResult>) {
        let (tx, rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join = thread::spawn(move || {
            run_live_analysis_worker(plugins_dir, memory_budget_bytes, rx, result_tx, worker_stop);
        });
        (
            Self {
                tx,
                stop,
                join: Some(join),
            },
            result_rx,
        )
    }

    pub fn configure(
        &self,
        epoch: u64,
        snapshot: LivePluginStateSnapshot,
        reason: PluginDiscontinuity,
    ) {
        let _ = self.tx.send(LiveAnalysisCommand::Configure {
            epoch,
            snapshot,
            reason,
        });
    }

    pub fn reload_plugins(
        &self,
        epoch: u64,
        snapshot: LivePluginStateSnapshot,
        reason: PluginDiscontinuity,
    ) {
        let _ = self.tx.send(LiveAnalysisCommand::Reload {
            epoch,
            snapshot,
            reason,
        });
    }

    pub fn analyze(&self, job: LiveAnalysisJob) {
        let _ = self.tx.send(LiveAnalysisCommand::Analyze(Box::new(job)));
    }

    pub fn discontinuity(&self, epoch: u64, reason: PluginDiscontinuity) {
        let _ = self
            .tx
            .send(LiveAnalysisCommand::Discontinuity { epoch, reason });
    }

    /// Drops every persistent context value held by the worker. Paired with
    /// an epoch bump so results computed against the old bus are discarded.
    pub fn clear_persistent(&self, epoch: u64) {
        let _ = self.tx.send(LiveAnalysisCommand::ClearPersistent { epoch });
    }

    pub fn set_memory_budget(&self, memory_budget_bytes: usize) {
        let _ = self
            .tx
            .send(LiveAnalysisCommand::SetMemoryBudget(memory_budget_bytes));
    }
}

impl Drop for LiveAnalysisWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.tx.send(LiveAnalysisCommand::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[derive(Debug, Clone)]
pub struct LiveAnalysisJob {
    pub epoch: u64,
    pub frame: PreviewFrame,
    pub global_settings_json: Option<Vec<u8>>,
    /// Full replacement snapshot of the persistent context bus. The GUI sends
    /// one when it owned the bus since the last live job (startup and
    /// paused-scrub → live transitions); otherwise the worker's own map stays
    /// authoritative so plugin-published values are never rolled back by a
    /// stale echo.
    pub persistent_seed: Option<HashMap<String, Vec<u8>>>,
    /// Host-authored upserts (`Some`) and removals (`None`) since the last
    /// dispatched job, applied on top of the worker's persistent map.
    pub persistent_updates: HashMap<String, Option<Vec<u8>>>,
    /// Highest host action request id published into this job's bus view.
    /// Echoed back through [`LiveAnalysisResult::action_request_watermark`].
    pub action_request_watermark: u64,
}

impl LiveAnalysisJob {
    /// Folds `next` into `self` when the worker coalesces queued jobs, so
    /// host-authored persistent updates from superseded jobs are not lost.
    fn coalesce_with(&mut self, next: LiveAnalysisJob) {
        let LiveAnalysisJob {
            epoch,
            frame,
            global_settings_json,
            persistent_seed,
            persistent_updates,
            action_request_watermark,
        } = next;
        self.epoch = epoch;
        self.frame = frame;
        self.global_settings_json = global_settings_json;
        if persistent_seed.is_some() {
            // A newer full snapshot already contains every older update.
            self.persistent_seed = persistent_seed;
            self.persistent_updates = persistent_updates;
        } else {
            self.persistent_updates.extend(persistent_updates);
        }
        self.action_request_watermark = self.action_request_watermark.max(action_request_watermark);
    }

    /// Applies this job's persistent-bus seed/updates to the worker map.
    fn apply_persistent_changes(&mut self, persistent_data: &mut HashMap<String, Vec<u8>>) {
        if let Some(seed) = self.persistent_seed.take() {
            *persistent_data = seed;
        }
        for (key, value) in self.persistent_updates.drain() {
            match value {
                Some(value) => {
                    persistent_data.insert(key, value);
                }
                None => {
                    persistent_data.remove(&key);
                }
            }
        }
    }
}

#[derive(Debug)]
enum LiveAnalysisCommand {
    Configure {
        epoch: u64,
        snapshot: LivePluginStateSnapshot,
        reason: PluginDiscontinuity,
    },
    Reload {
        epoch: u64,
        snapshot: LivePluginStateSnapshot,
        reason: PluginDiscontinuity,
    },
    Discontinuity {
        epoch: u64,
        reason: PluginDiscontinuity,
    },
    ClearPersistent {
        epoch: u64,
    },
    Analyze(Box<LiveAnalysisJob>),
    SetMemoryBudget(usize),
    Stop,
}

#[derive(Debug, Clone)]
enum PluginEventFrameData {
    Upstream {
        source: LiveEventSource,
        event_range: Range<u64>,
    },
    Inline(Box<[FfiCdEvent]>),
}

#[derive(Debug, Clone)]
struct PluginEventFrame {
    data: PluginEventFrameData,
    window_start_us: u64,
    window_end_us: u64,
    byte_len: usize,
}

#[derive(Debug)]
pub struct PluginEventHistory {
    frames: VecDeque<PluginEventFrame>,
    memory_budget_bytes: usize,
    memory_usage_bytes: usize,
    upstream: Option<LiveEventSource>,
    upstream_cursor: Option<CursorId>,
    /// True when this history registered `upstream_cursor` itself. Borrowed
    /// cursors (e.g. the pipeline controller's) stay registered on detach so
    /// their owner can re-attach without a stale id.
    owns_upstream_cursor: bool,
}

impl Default for PluginEventHistory {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
            memory_budget_bytes: DEFAULT_EVENT_HISTORY_BUDGET_BYTES,
            memory_usage_bytes: 0,
            upstream: None,
            upstream_cursor: None,
            owns_upstream_cursor: false,
        }
    }
}

impl PluginEventHistory {
    pub fn attach_upstream(&mut self, source: LiveEventSource, cursor: Option<CursorId>) {
        if self
            .upstream
            .as_ref()
            .is_some_and(|existing| existing.ptr_eq(&source))
        {
            if self.upstream_cursor.is_none() {
                match cursor {
                    Some(cursor) => {
                        self.upstream_cursor = Some(cursor);
                        self.owns_upstream_cursor = false;
                    }
                    None => {
                        self.upstream_cursor = Some(register_plugin_cursor(&source));
                        self.owns_upstream_cursor = true;
                    }
                }
            }
            return;
        }

        self.detach_upstream();
        let owns_cursor = cursor.is_none();
        let cursor = cursor.unwrap_or_else(|| register_plugin_cursor(&source));
        self.upstream = Some(source);
        self.upstream_cursor = Some(cursor);
        self.owns_upstream_cursor = owns_cursor;
    }

    pub fn detach_upstream(&mut self) {
        if let (Some(source), Some(cursor)) = (&self.upstream, self.upstream_cursor) {
            if self.owns_upstream_cursor {
                source.unregister_cursor(cursor);
            }
        }
        self.upstream = None;
        self.upstream_cursor = None;
        self.owns_upstream_cursor = false;
    }

    pub fn sync_from_upstream(&mut self) -> Result<(), String> {
        let (Some(source), Some(cursor)) = (&self.upstream, self.upstream_cursor) else {
            return Ok(());
        };
        let batches = source
            .drain_cursor_frames(cursor)
            .map_err(|err| format!("plugin retained history fell behind upstream events: {err}"))?;
        for batch in batches {
            self.push_upstream_batch(batch);
        }
        Ok(())
    }

    pub fn push_frame(&mut self, frame: &PreviewFrame) {
        let Some(event_count) = frame.event_count() else {
            return;
        };
        if event_count == 0 {
            return;
        }

        let data = match (frame.event_source.as_ref(), frame.event_range.as_ref()) {
            (Some(source), Some(event_range)) => PluginEventFrameData::Upstream {
                source: source.clone(),
                event_range: event_range.clone(),
            },
            (None, None) => {
                let Some(events) = frame.events_snapshot() else {
                    return;
                };
                PluginEventFrameData::Inline(
                    events
                        .iter()
                        .copied()
                        .map(FfiCdEvent::from)
                        .collect::<Box<[FfiCdEvent]>>(),
                )
            }
            _ => {
                eprintln!("inconsistent upstream metadata: event_source and event_range must both be present or both absent");
                return;
            }
        };

        let byte_len = event_count.saturating_mul(std::mem::size_of::<FfiCdEvent>());
        self.frames.push_back(PluginEventFrame {
            data,
            window_start_us: frame.window_start_us,
            window_end_us: frame.window_end_us,
            byte_len,
        });
        self.memory_usage_bytes = self.memory_usage_bytes.saturating_add(byte_len);
        self.enforce_memory_budget();
    }

    fn push_upstream_batch(&mut self, batch: LiveEventFrameBatch) {
        if batch.events.is_empty() {
            return;
        }
        let byte_len = batch
            .events
            .len()
            .saturating_mul(std::mem::size_of::<FfiCdEvent>());
        self.frames.push_back(PluginEventFrame {
            data: PluginEventFrameData::Inline(batch.events.into_boxed_slice()),
            window_start_us: batch.window_start_us,
            window_end_us: batch.window_end_us,
            byte_len,
        });
        self.memory_usage_bytes = self.memory_usage_bytes.saturating_add(byte_len);
        self.enforce_memory_budget();
    }

    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    pub fn oldest_timestamp_us(&self) -> Option<u64> {
        self.frames.front().map(|frame| frame.window_start_us)
    }

    pub fn frame_window(&self, index: usize) -> Option<(u64, u64)> {
        let frame = self.frames.get(index)?;
        Some((frame.window_start_us, frame.window_end_us))
    }

    pub fn materialize_frame(&self, index: usize) -> Option<Box<[FfiCdEvent]>> {
        let frame = self.frames.get(index)?;
        match &frame.data {
            PluginEventFrameData::Upstream {
                source,
                event_range,
            } => {
                let events = source.compact_events_for_range(event_range.clone())?;
                Some(events.into_boxed_slice())
            }
            PluginEventFrameData::Inline(events) => Some(events.clone()),
        }
    }

    pub fn frame_range_for_timestamps(
        &self,
        start_timestamp_us: u64,
        end_timestamp_us: u64,
    ) -> Option<(usize, usize)> {
        debug_assert!(
            self.frames
                .iter()
                .zip(self.frames.iter().skip(1))
                .all(
                    |(previous, next)| previous.window_start_us <= next.window_start_us
                        && previous.window_end_us <= next.window_end_us
                ),
            "plugin event history must remain monotonic across range queries"
        );
        if self.frames.is_empty() || start_timestamp_us > end_timestamp_us {
            return None;
        }

        let start_index = self.first_frame_with_window_end_at_or_after(start_timestamp_us)?;
        let end_index = self.first_frame_with_window_start_after(end_timestamp_us);
        if start_index >= end_index {
            None
        } else {
            Some((start_index, end_index))
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.memory_usage_bytes = 0;
    }

    pub fn set_memory_budget(&mut self, memory_budget_bytes: usize) {
        self.memory_budget_bytes = memory_budget_bytes;
        self.enforce_memory_budget();
    }

    pub fn memory_budget_bytes(&self) -> usize {
        self.memory_budget_bytes
    }

    pub fn memory_usage_bytes(&self) -> usize {
        self.memory_usage_bytes
    }

    fn first_frame_with_window_end_at_or_after(&self, timestamp_us: u64) -> Option<usize> {
        let mut left = 0usize;
        let mut right = self.frames.len();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.frames[mid].window_end_us < timestamp_us {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        (left < self.frames.len()).then_some(left)
    }

    fn first_frame_with_window_start_after(&self, timestamp_us: u64) -> usize {
        let mut left = 0usize;
        let mut right = self.frames.len();
        while left < right {
            let mid = left + (right - left) / 2;
            if self.frames[mid].window_start_us <= timestamp_us {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        left
    }

    fn enforce_memory_budget(&mut self) {
        while self.frames.len() > 1 && self.memory_usage_bytes > self.memory_budget_bytes {
            let Some(frame) = self.frames.pop_front() else {
                break;
            };
            self.memory_usage_bytes = self.memory_usage_bytes.saturating_sub(frame.byte_len);
        }
    }
}

impl Drop for PluginEventHistory {
    fn drop(&mut self) {
        self.detach_upstream();
    }
}

fn register_plugin_cursor(source: &LiveEventSource) -> CursorId {
    source.register_cursor(
        "plugin-runtime",
        CursorPolicy::Lossless {
            backpressure: BackpressureBehavior::FailLoud,
        },
    )
}

#[derive(Debug, Clone)]
struct CachedSettingValue {
    fetched_at: Instant,
    value: Option<Value>,
}

#[derive(Debug, Clone)]
struct CachedStatusEntries {
    fetched_at: Instant,
    entries: Vec<StatusEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub domain: Option<String>,
    pub library: Option<String>,
}

#[derive(Debug, Clone)]
struct PluginSpec {
    entry_dir: PathBuf,
    manifest: Option<PluginManifest>,
    display_name: String,
}

impl PluginSpec {
    fn manifest(&self) -> Result<&PluginManifest, String> {
        self.manifest
            .as_ref()
            .ok_or_else(|| format!("plugin manifest missing for {}", self.entry_dir.display()))
    }
}

pub struct DynPlugin {
    _lib: Library,
    vtable: PluginVTable,
    instance: *mut c_void,
    manifest: PluginManifest,
    cached_name: String,
    cached_description: String,
    cached_schema: SettingsSchema,
    cached_dependencies: Vec<String>,
    input_kind: PluginInput,
    state_kind: PluginStateKind,
    capabilities: PluginCapabilities,
    cached_setting_values: HashMap<String, CachedSettingValue>,
    cached_status_entries: Option<CachedStatusEntries>,
    ui_dirty: bool,
}

impl DynPlugin {
    fn load(spec: &PluginSpec) -> Result<Self, String> {
        let manifest = spec.manifest()?;
        let library_path = find_library_file(&spec.entry_dir, manifest)?;
        let lib = unsafe { Library::new(&library_path) }
            .map_err(|err| format!("loading {} failed: {err}", library_path.display()))?;
        let entry = unsafe { lib.get::<PluginEntry>(b"augur_plugin_vtable\0") }
            .map_err(|err| format!("loading symbol {PLUGIN_ENTRY_SYMBOL} failed: {err}"))?;
        let vtable = validate_plugin_vtable(unsafe { entry() })?;
        let instance = unsafe { (vtable.create)() };
        if instance.is_null() {
            return Err("plugin create() returned a null instance".into());
        }

        let mut plugin = Self {
            _lib: lib,
            vtable,
            instance,
            manifest: manifest.clone(),
            cached_name: String::new(),
            cached_description: String::new(),
            cached_schema: SettingsSchema::default(),
            cached_dependencies: Vec::new(),
            input_kind: PluginInput::FrameOnly,
            state_kind: PluginStateKind::default(),
            capabilities: PluginCapabilities::default(),
            cached_setting_values: HashMap::new(),
            cached_status_entries: None,
            ui_dirty: false,
        };
        if let Err(err) = plugin.refresh_cached_metadata() {
            unsafe {
                (plugin.vtable.destroy)(plugin.instance);
            }
            return Err(err);
        }
        Ok(plugin)
    }

    fn refresh_cached_metadata(&mut self) -> Result<(), String> {
        let name = self
            .call_string(self.vtable.name)?
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.manifest.name.clone());
        let description = self
            .call_string(self.vtable.description)?
            .or_else(|| self.manifest.description.clone())
            .unwrap_or_default();
        let schema = self.read_json(self.vtable.settings_schema)?;
        let input_kind = unsafe { (self.vtable.input_kind)(self.instance) };
        let state_kind = unsafe { (self.vtable.plugin_state_kind)(self.instance) };
        let capabilities = unsafe { (self.vtable.capabilities)(self.instance) };
        let dependency_count = unsafe { (self.vtable.num_dependencies)(self.instance) };
        let mut dependencies = Vec::with_capacity(dependency_count);
        for index in 0..dependency_count {
            if let Some(dependency) = self.call_dependency(index)? {
                dependencies.push(dependency);
            }
        }

        self.cached_name = name;
        self.cached_description = description;
        self.cached_schema = schema;
        self.cached_dependencies = dependencies;
        self.input_kind = input_kind;
        self.state_kind = state_kind;
        self.capabilities = capabilities;
        self.invalidate_ui_cache();
        Ok(())
    }

    fn call_string(
        &self,
        func: unsafe extern "C" fn(*const c_void) -> FfiString,
    ) -> Result<Option<String>, String> {
        let ffi = unsafe { func(self.instance) };
        ffi_string_to_option(ffi, "plugin returned invalid UTF-8")
    }

    fn call_dependency(&self, index: usize) -> Result<Option<String>, String> {
        let ffi = unsafe { (self.vtable.dependency)(self.instance, index) };
        ffi_string_to_option(
            ffi,
            &format!("plugin dependency {index} is not valid UTF-8"),
        )
    }

    fn read_json<T: serde::de::DeserializeOwned>(
        &self,
        func: unsafe extern "C" fn(*const c_void, *mut *const u8, *mut usize),
    ) -> Result<T, String> {
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        unsafe {
            func(self.instance, &mut out_ptr, &mut out_len);
        }
        let bytes = unsafe { bytes_from_out_ptr(out_ptr, out_len) };
        serde_json::from_slice(bytes)
            .map_err(|err| format!("plugin returned invalid JSON payload: {err}"))
    }

    pub fn name(&self) -> &str {
        &self.cached_name
    }

    pub fn description(&self) -> &str {
        &self.cached_description
    }

    pub fn input_kind(&self) -> PluginInput {
        self.input_kind
    }

    pub fn plugin_state_kind(&self) -> PluginStateKind {
        self.state_kind
    }

    pub fn capabilities(&self) -> PluginCapabilities {
        self.capabilities
    }

    pub fn dependencies(&self) -> &[String] {
        &self.cached_dependencies
    }

    pub fn settings_schema(&self) -> &SettingsSchema {
        &self.cached_schema
    }

    pub fn enabled(&self) -> bool {
        unsafe { (self.vtable.enabled)(self.instance) }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        let changed = self.enabled() != enabled;
        unsafe {
            (self.vtable.set_enabled)(self.instance, enabled);
        }
        self.invalidate_ui_cache();
        self.ui_dirty |= changed;
    }

    pub fn reset(&mut self) {
        unsafe {
            (self.vtable.reset)(self.instance);
        }
        self.invalidate_ui_cache();
        self.ui_dirty = false;
    }

    pub fn on_discontinuity(&mut self, reason: PluginDiscontinuity) {
        unsafe {
            (self.vtable.on_discontinuity)(self.instance, reason);
        }
        self.invalidate_ui_cache();
        self.ui_dirty = false;
    }

    pub fn get_setting_value(&self, key: &str) -> Result<Option<Value>, String> {
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        let found = unsafe {
            (self.vtable.get_setting)(
                self.instance,
                FfiString::from(key),
                &mut out_ptr,
                &mut out_len,
            )
        };
        if !found {
            return Ok(None);
        }
        let bytes = unsafe { bytes_from_out_ptr(out_ptr, out_len) };
        serde_json::from_slice(bytes)
            .map(Some)
            .map_err(|err| format!("plugin setting {key} is not valid JSON: {err}"))
    }

    pub fn get_setting_value_cached(&mut self, key: &str) -> Result<Option<Value>, String> {
        let now = Instant::now();
        if let Some(cached) = self.cached_setting_values.get(key) {
            if now.duration_since(cached.fetched_at) < PLUGIN_UI_CACHE_INTERVAL {
                return Ok(cached.value.clone());
            }
        }

        let value = self.get_setting_value(key)?;
        self.cached_setting_values.insert(
            key.to_owned(),
            CachedSettingValue {
                fetched_at: now,
                value: value.clone(),
            },
        );
        Ok(value)
    }

    pub fn set_setting_value(&mut self, key: &str, value: &Value) -> Result<bool, String> {
        let json = serde_json::to_vec(value)
            .map_err(|err| format!("serializing setting failed: {err}"))?;
        let updated = unsafe {
            (self.vtable.set_setting)(
                self.instance,
                FfiString::from(key),
                FfiSlice::from_slice(&json),
            )
        };
        if updated {
            self.invalidate_ui_cache();
            self.ui_dirty = true;
        }
        Ok(updated)
    }

    pub fn is_dirty(&self) -> bool {
        self.ui_dirty
    }

    pub fn status_entries(&self) -> Result<Vec<StatusEntry>, String> {
        self.read_json(self.vtable.status_entries)
    }

    pub fn status_entries_cached(&mut self) -> Result<Vec<StatusEntry>, String> {
        let now = Instant::now();
        if let Some(cached) = &self.cached_status_entries {
            if now.duration_since(cached.fetched_at) < PLUGIN_UI_CACHE_INTERVAL {
                return Ok(cached.entries.clone());
            }
        }

        let entries = self.status_entries()?;
        self.cached_status_entries = Some(CachedStatusEntries {
            fetched_at: now,
            entries: entries.clone(),
        });
        Ok(entries)
    }

    pub fn host_views(&self) -> Result<HostViewRegistry, String> {
        self.read_json(self.vtable.host_views)
    }

    pub fn host_view_dataset(&self, dataset_id: &str) -> Result<Option<Vec<u8>>, String> {
        let bytes = self.call_optional_bytes(|inst, ptr, len| unsafe {
            (self.vtable.host_view_dataset)(inst, FfiString::from(dataset_id), ptr, len)
        });
        Ok(bytes.map(|b| b.to_vec()))
    }

    pub fn host_view_dataset_generation(&self, dataset_id: &str) -> u64 {
        unsafe {
            (self.vtable.host_view_dataset_generation)(self.instance, FfiString::from(dataset_id))
        }
    }

    pub fn invalidate_ui_cache(&mut self) {
        self.cached_setting_values.clear();
        self.cached_status_entries = None;
    }

    fn call_optional_bytes(
        &self,
        call: impl FnOnce(*const c_void, *mut *const u8, *mut usize) -> bool,
    ) -> Option<&[u8]> {
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        if !call(self.instance, &mut out_ptr, &mut out_len) {
            return None;
        }
        Some(unsafe { bytes_from_out_ptr(out_ptr, out_len) })
    }

    pub fn process_frame(
        &mut self,
        frame: &PreviewFrame,
        raw_events: &[FfiCdEvent],
        pass: &AnalysisPassContext<'_>,
        analysis_output: &mut AnalysisOutput,
        context_data: &mut HashMap<String, Vec<u8>>,
        persistent_data: &mut HashMap<String, Vec<u8>>,
    ) {
        let event_store = pass.event_store;
        let history_cache = pass.history_cache;
        let ffi_frame = FfiPreviewFrame {
            width: frame.width,
            height: frame.height,
            pixels: FfiSlice::from_slice(&frame.pixels),
            events: FfiSlice::from_slice(raw_events),
            window_start_us: frame.window_start_us,
            window_end_us: frame.window_end_us,
        };

        let mut output_bridge = OutputBridge {
            output: analysis_output,
        };
        let mut context_bridge = ContextBridge {
            data: context_data,
            persistent_data,
        };
        let store_bridge = EventStoreBridge {
            store: event_store,
            materialized_frames: &history_cache.frames,
        };
        let mut callbacks = FfiOutputCallbacks {
            ctx: (&mut output_bridge as *mut OutputBridge).cast(),
            add_highlight_pixels,
            add_crosshair_markers,
            add_marker_overlay,
            add_warning,
        };
        let mut plugin_context = FfiPluginContext {
            ctx: (&mut context_bridge as *mut ContextBridge).cast(),
            raw_events: FfiSlice::from_slice(raw_events),
            publish: publish_context_value,
            get: get_context_value,
            publish_persistent: publish_persistent_context_value,
            get_persistent: get_persistent_context_value,
        };
        let ffi_event_store = FfiEventStoreHandle {
            ctx: (&store_bridge as *const EventStoreBridge).cast(),
            frame_count: frame_count_callback,
            frame_at: frame_at_callback,
            frame_range_for_timestamps: frame_range_for_timestamps_callback,
            oldest_timestamp_us: oldest_timestamp_callback,
        };

        self.ui_dirty = false;
        unsafe {
            (self.vtable.process_frame)(
                self.instance,
                &ffi_frame,
                &mut callbacks,
                &mut plugin_context,
                &ffi_event_store,
            );
        }
    }
}

fn validate_plugin_vtable(vtable_ptr: *const PluginVTable) -> Result<PluginVTable, String> {
    if vtable_ptr.is_null() {
        return Err("plugin entry returned a null vtable pointer".into());
    }

    let expected_size = std::mem::size_of::<PluginVTable>();
    let reported_size = unsafe { (*vtable_ptr).vtable_size };
    if reported_size != expected_size {
        return Err(format!(
            "plugin vtable size mismatch (plugin: {reported_size}, host: {expected_size}) — \
             rebuild the plugin against the current augur-plugin-api"
        ));
    }

    let reported_abi = unsafe { (*vtable_ptr).abi_version };
    if reported_abi != PLUGIN_ABI_VERSION {
        return Err(format!(
            "plugin ABI mismatch (plugin: {reported_abi}, host: {PLUGIN_ABI_VERSION}) — \
             rebuild the plugin against the current augur-plugin-api"
        ));
    }

    let vtable = unsafe { *vtable_ptr };
    if !plugin_vtable_looks_plausible(&vtable) {
        return Err(
            "plugin vtable contains invalid function pointers — rebuild the plugin against the \
             current augur-plugin-api"
                .into(),
        );
    }

    Ok(vtable)
}

fn plugin_vtable_looks_plausible(vtable: &PluginVTable) -> bool {
    [
        vtable.create as usize,
        vtable.destroy as usize,
        vtable.name as usize,
        vtable.description as usize,
        vtable.enabled as usize,
        vtable.set_enabled as usize,
        vtable.reset as usize,
        vtable.on_discontinuity as usize,
        vtable.input_kind as usize,
        vtable.capabilities as usize,
        vtable.plugin_state_kind as usize,
        vtable.num_dependencies as usize,
        vtable.dependency as usize,
        vtable.process_frame as usize,
        vtable.settings_schema as usize,
        vtable.get_setting as usize,
        vtable.set_setting as usize,
        vtable.status_entries as usize,
        vtable.host_views as usize,
        vtable.host_view_dataset as usize,
        vtable.host_view_dataset_generation as usize,
    ]
    .into_iter()
    .all(|address| address >= MIN_PLAUSIBLE_FUNCTION_POINTER)
}

impl Drop for DynPlugin {
    fn drop(&mut self) {
        unsafe {
            (self.vtable.destroy)(self.instance);
        }
    }
}

pub struct ManagedPlugin {
    spec: PluginSpec,
    plugin: Option<DynPlugin>,
    load_error: Option<String>,
}

impl ManagedPlugin {
    pub fn plugin(&self) -> Option<&DynPlugin> {
        self.plugin.as_ref()
    }

    pub fn plugin_mut(&mut self) -> Option<&mut DynPlugin> {
        self.plugin.as_mut()
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn name(&self) -> &str {
        self.plugin
            .as_ref()
            .map(|plugin| plugin.name())
            .or_else(|| {
                self.spec
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.name.as_str())
            })
            .unwrap_or(&self.spec.display_name)
    }

    pub fn description(&self) -> &str {
        self.plugin
            .as_ref()
            .map(|plugin| plugin.description())
            .or_else(|| {
                self.spec
                    .manifest
                    .as_ref()
                    .and_then(|manifest| manifest.description.as_deref())
            })
            .unwrap_or("")
    }

    pub fn domain(&self) -> &str {
        self.spec
            .manifest
            .as_ref()
            .and_then(|manifest| manifest.domain.as_deref())
            .unwrap_or("-")
    }

    pub fn version(&self) -> &str {
        self.spec
            .manifest
            .as_ref()
            .map(|manifest| manifest.version.as_str())
            .unwrap_or("-")
    }

    pub fn status_label(&self) -> &'static str {
        if self.plugin.is_some() {
            "loaded"
        } else {
            "error"
        }
    }

    pub fn phase_label(&self) -> &'static str {
        self.plugin
            .as_ref()
            .map(|plugin| plugin_phase_label(plugin.input_kind()))
            .unwrap_or("-")
    }
}

pub struct PluginManager {
    plugins_dir: PathBuf,
    records: Vec<ManagedPlugin>,
}

impl PluginManager {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self {
            plugins_dir,
            records: Vec::new(),
        }
    }

    pub fn new_default() -> Self {
        Self::new(default_plugins_dir())
    }

    pub fn plugins_dir(&self) -> &Path {
        &self.plugins_dir
    }

    pub fn records(&self) -> &[ManagedPlugin] {
        &self.records
    }

    pub fn records_mut(&mut self) -> &mut [ManagedPlugin] {
        &mut self.records
    }

    pub fn scan_and_load(&mut self) -> Result<(), String> {
        fs::create_dir_all(&self.plugins_dir)
            .map_err(|err| format!("creating plugin directory failed: {err}"))?;

        let mut records = Vec::new();
        let entries = fs::read_dir(&self.plugins_dir)
            .map_err(|err| format!("reading plugin directory failed: {err}"))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("reading plugin entry failed: {err}"))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            records.push(load_record(path));
        }
        records.sort_by_key(|record| record.name().to_ascii_lowercase());
        self.records = records;
        Ok(())
    }

    pub fn reload_plugin(&mut self, index: usize) -> Result<(), String> {
        let Some(record) = self.records.get_mut(index) else {
            return Err(format!("plugin index {index} is out of range"));
        };
        let refreshed = load_record(record.spec.entry_dir.clone());
        let result = match &refreshed.load_error {
            Some(err) => Err(err.clone()),
            None => Ok(()),
        };
        *record = refreshed;
        result
    }
    pub fn open_plugins_dir(&self) -> Result<(), String> {
        fs::create_dir_all(&self.plugins_dir)
            .map_err(|err| format!("creating plugin directory failed: {err}"))?;
        open_directory(&self.plugins_dir)
    }
}

fn run_live_analysis_worker(
    plugins_dir: PathBuf,
    memory_budget_bytes: usize,
    rx: mpsc::Receiver<LiveAnalysisCommand>,
    result_tx: mpsc::Sender<LiveAnalysisResult>,
    stop: Arc<AtomicBool>,
) {
    let mut manager = PluginManager::new(plugins_dir);
    let mut load_warning = manager.scan_and_load().err();
    let mut event_store = PluginEventHistory::default();
    event_store.set_memory_budget(memory_budget_bytes);
    let mut persistent_data = HashMap::new();
    let mut host_snapshot_cache = HostSnapshotCache::default();
    let mut active_epoch = 0u64;

    while !stop.load(Ordering::Relaxed) {
        let Ok(command) = rx.recv() else {
            break;
        };
        match command {
            LiveAnalysisCommand::Stop => break,
            LiveAnalysisCommand::SetMemoryBudget(bytes) => {
                event_store.set_memory_budget(bytes);
            }
            LiveAnalysisCommand::Configure {
                epoch,
                snapshot,
                reason,
            } => {
                active_epoch = active_epoch.max(epoch);
                apply_live_plugin_snapshot(&mut manager, &snapshot);
                notify_plugins_of_discontinuity(&mut manager, reason);
                host_snapshot_cache.clear();
                event_store.clear();
            }
            LiveAnalysisCommand::Reload {
                epoch,
                snapshot,
                reason,
            } => {
                active_epoch = active_epoch.max(epoch);
                load_warning = manager.scan_and_load().err();
                apply_live_plugin_snapshot(&mut manager, &snapshot);
                notify_plugins_of_discontinuity(&mut manager, reason);
                host_snapshot_cache.clear();
                event_store.clear();
            }
            LiveAnalysisCommand::Discontinuity { epoch, reason } => {
                active_epoch = active_epoch.max(epoch);
                event_store.clear();
                notify_plugins_of_discontinuity(&mut manager, reason);
                host_snapshot_cache.clear();
            }
            LiveAnalysisCommand::ClearPersistent { epoch } => {
                active_epoch = active_epoch.max(epoch);
                persistent_data.clear();
            }
            LiveAnalysisCommand::Analyze(mut job) => {
                while let Ok(next) = rx.try_recv() {
                    match next {
                        LiveAnalysisCommand::Stop => return,
                        LiveAnalysisCommand::SetMemoryBudget(bytes) => {
                            event_store.set_memory_budget(bytes);
                        }
                        LiveAnalysisCommand::Configure {
                            epoch,
                            snapshot,
                            reason,
                        } => {
                            active_epoch = active_epoch.max(epoch);
                            apply_live_plugin_snapshot(&mut manager, &snapshot);
                            notify_plugins_of_discontinuity(&mut manager, reason);
                            host_snapshot_cache.clear();
                            event_store.clear();
                        }
                        LiveAnalysisCommand::Reload {
                            epoch,
                            snapshot,
                            reason,
                        } => {
                            active_epoch = active_epoch.max(epoch);
                            load_warning = manager.scan_and_load().err();
                            apply_live_plugin_snapshot(&mut manager, &snapshot);
                            notify_plugins_of_discontinuity(&mut manager, reason);
                            host_snapshot_cache.clear();
                            event_store.clear();
                        }
                        LiveAnalysisCommand::Discontinuity { epoch, reason } => {
                            active_epoch = active_epoch.max(epoch);
                            event_store.clear();
                            notify_plugins_of_discontinuity(&mut manager, reason);
                            host_snapshot_cache.clear();
                        }
                        LiveAnalysisCommand::ClearPersistent { epoch } => {
                            active_epoch = active_epoch.max(epoch);
                            persistent_data.clear();
                            // The pending job's merged seed/updates predate
                            // the clear and must not resurrect old values.
                            job.persistent_seed = None;
                            job.persistent_updates.clear();
                        }
                        LiveAnalysisCommand::Analyze(next_job) => {
                            job.coalesce_with(*next_job);
                        }
                    }
                }

                if job.epoch < active_epoch {
                    continue;
                }
                active_epoch = active_epoch.max(job.epoch);
                job.apply_persistent_changes(&mut persistent_data);
                let mut result = process_live_analysis_job(
                    &mut manager,
                    &mut event_store,
                    &mut persistent_data,
                    &mut host_snapshot_cache,
                    &job,
                );
                if let Some(warning) = load_warning.take() {
                    result.output.warnings.push(AnalysisWarning {
                        source: "plugin-runtime".into(),
                        severity: CoreSeverity::Warning,
                        message: warning,
                    });
                }
                if result_tx.send(result).is_err() {
                    break;
                }
            }
        }
    }
    event_store.detach_upstream();
}

fn process_live_analysis_job(
    manager: &mut PluginManager,
    event_store: &mut PluginEventHistory,
    persistent_data: &mut HashMap<String, Vec<u8>>,
    host_snapshot_cache: &mut HostSnapshotCache,
    job: &LiveAnalysisJob,
) -> LiveAnalysisResult {
    let mut output = AnalysisOutput::default();
    let mut context_data = HashMap::new();
    if let Some(json) = &job.global_settings_json {
        context_data.insert("augur.global_settings".to_owned(), json.clone());
    }

    let retained_history_needed = manager.records().iter().any(|record| {
        record
            .plugin()
            .is_some_and(|plugin| plugin.enabled() && plugin.capabilities().retained_event_history)
    });
    let raw_events_needed = manager.records().iter().any(|record| {
        record
            .plugin()
            .is_some_and(|plugin| plugin.enabled() && plugin.input_kind() == PluginInput::RawEvents)
    });

    if retained_history_needed {
        if let Some(source) = job.frame.event_source.clone() {
            event_store.attach_upstream(source, None);
        }
        if let Err(err) = event_store.sync_from_upstream() {
            output.warnings.push(AnalysisWarning {
                source: "plugin-runtime".into(),
                severity: CoreSeverity::Warning,
                message: err,
            });
            event_store.clear();
            notify_plugins_of_discontinuity(manager, PluginDiscontinuity::HistoryEvicted);
            host_snapshot_cache.clear();
        }
    } else {
        event_store.detach_upstream();
        event_store.clear();
    }

    let raw_events = if raw_events_needed {
        job.frame.compact_events_snapshot().unwrap_or_default()
    } else {
        Vec::new()
    };

    let history_cache = EventHistoryMaterializationCache::default();
    let pass = AnalysisPassContext {
        event_store,
        history_cache: &history_cache,
    };
    for phase in [
        PluginInput::FrameOnly,
        PluginInput::RawEvents,
        PluginInput::DerivedData,
    ] {
        for record in manager.records_mut() {
            let Some(plugin) = record.plugin_mut() else {
                continue;
            };
            if plugin.enabled() && plugin.input_kind() == phase {
                // Only RawEvents-phase plugins receive the current frame's raw
                // events — matching the offline pipeline, so declaring an
                // input kind (not another plugin's needs) decides visibility.
                plugin.process_frame(
                    &job.frame,
                    if phase == PluginInput::RawEvents {
                        &raw_events
                    } else {
                        &[]
                    },
                    &pass,
                    &mut output,
                    &mut context_data,
                    persistent_data,
                );
            }
        }
    }

    LiveAnalysisResult {
        epoch: job.epoch,
        output,
        context_data,
        persistent_data: persistent_data.clone(),
        host_snapshots: collect_live_host_snapshots(manager, host_snapshot_cache),
        action_request_watermark: job.action_request_watermark,
    }
}

fn apply_live_plugin_snapshot(manager: &mut PluginManager, snapshot: &LivePluginStateSnapshot) {
    for state in &snapshot.plugins {
        let Some(plugin) = manager
            .records_mut()
            .iter_mut()
            .filter_map(ManagedPlugin::plugin_mut)
            .find(|plugin| plugin.name() == state.name)
        else {
            continue;
        };
        plugin.set_enabled(state.enabled);
        for (key, value) in &state.settings {
            let _ = plugin.set_setting_value(key, value);
        }
    }
}

/// Delivers a discontinuity to every loaded plugin. The default trait
/// implementation resets only `Accumulating` plugins; `Stateless` plugins
/// that override `on_discontinuity` still get to observe the boundary.
fn notify_plugins_of_discontinuity(manager: &mut PluginManager, reason: PluginDiscontinuity) {
    for record in manager.records_mut() {
        let Some(plugin) = record.plugin_mut() else {
            continue;
        };
        plugin.on_discontinuity(reason);
    }
}

fn collect_live_host_snapshots(
    manager: &PluginManager,
    cache: &mut HostSnapshotCache,
) -> Vec<LivePluginHostSnapshot> {
    manager
        .records()
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            let plugin = record.plugin()?;
            if !plugin.enabled() {
                return None;
            }
            let name = plugin.name().to_owned();
            match plugin.host_views() {
                Ok(registry) => {
                    let datasets = registry
                        .datasets
                        .iter()
                        .map(|descriptor| {
                            let generation = plugin.host_view_dataset_generation(&descriptor.id);
                            let payload =
                                cache.resolve((index, descriptor.id.clone()), generation, || {
                                    match plugin.host_view_dataset(&descriptor.id) {
                                        Ok(Some(bytes)) => {
                                            Some(decode_dataset_snapshot(descriptor, &bytes))
                                        }
                                        Ok(None) => None,
                                        Err(err) => Some(Err(err)),
                                    }
                                });
                            LiveHostDatasetSnapshot {
                                id: descriptor.id.clone(),
                                generation,
                                payload,
                            }
                        })
                        .collect();
                    Some(LivePluginHostSnapshot {
                        index,
                        name,
                        registry: Some(registry),
                        datasets,
                        warning: None,
                    })
                }
                Err(err) => Some(LivePluginHostSnapshot {
                    index,
                    name: name.clone(),
                    registry: None,
                    datasets: Vec::new(),
                    warning: Some(format!("Failed to read host views from {name}: {err}")),
                }),
            }
        })
        .collect()
}

struct OutputBridge<'a> {
    output: &'a mut AnalysisOutput,
}

struct ContextBridge<'a> {
    data: &'a mut HashMap<String, Vec<u8>>,
    persistent_data: &'a mut HashMap<String, Vec<u8>>,
}

/// Caches ring-backed history frames materialized for plugin FFI reads,
/// shared by every plugin call of one analysis pass so N retained-history
/// plugins decode each frame once instead of N times.
///
/// # Safety
/// `!Sync` by construction (`UnsafeCell`); only accessed from
/// `frame_at_callback`, which runs synchronously and non-reentrantly during
/// `process_frame`. Callers must use a fresh cache per analysis pass —
/// frame indexes shift when the history evicts between passes.
#[derive(Default)]
pub struct EventHistoryMaterializationCache {
    frames: UnsafeCell<HashMap<usize, Box<[FfiCdEvent]>>>,
}

/// Retained-history inputs shared by every plugin call of one analysis pass.
pub struct AnalysisPassContext<'a> {
    pub event_store: &'a PluginEventHistory,
    pub history_cache: &'a EventHistoryMaterializationCache,
}

struct EventStoreBridge<'a> {
    store: &'a PluginEventHistory,
    materialized_frames: &'a UnsafeCell<HashMap<usize, Box<[FfiCdEvent]>>>,
}

unsafe extern "C" fn add_highlight_pixels(
    ctx: *mut c_void,
    pixels: FfiSlice<FfiPixel>,
    color: FfiColorRgba,
) {
    let Some(bridge) = ctx.cast::<OutputBridge>().as_mut() else {
        return;
    };
    let pixels = pixels
        .as_slice()
        .iter()
        .map(|pixel| Pixel {
            x: pixel.x,
            y: pixel.y,
        })
        .collect();
    bridge.output.overlays.push(Overlay::HighlightPixels {
        pixels,
        color: color.to_rgba(),
    });
}

unsafe extern "C" fn add_crosshair_markers(
    ctx: *mut c_void,
    markers: FfiSlice<FfiSubpixelMarker>,
    color: FfiColorRgba,
    arm_len: u16,
) {
    let Some(bridge) = ctx.cast::<OutputBridge>().as_mut() else {
        return;
    };
    let markers = markers
        .as_slice()
        .iter()
        .map(|marker| SubpixelMarker {
            x: marker.x,
            y: marker.y,
        })
        .collect();
    bridge.output.overlays.push(Overlay::CrosshairMarkers {
        markers,
        color: color.to_rgba(),
        arm_len,
    });
}

unsafe extern "C" fn add_marker_overlay(
    ctx: *mut c_void,
    markers: FfiSlice<FfiMarkerOverlayItem>,
    dataset_id: FfiString,
    layer_id: FfiString,
    source_label: FfiString,
) {
    let Some(bridge) = ctx.cast::<OutputBridge>().as_mut() else {
        return;
    };
    let Ok(dataset_id) = ffi_string_to_option(dataset_id, "marker overlay dataset_id") else {
        return;
    };
    let Ok(layer_id) = ffi_string_to_option(layer_id, "marker overlay layer_id") else {
        return;
    };
    let Ok(source_label) = ffi_string_to_option(source_label, "marker overlay source_label") else {
        return;
    };

    let markers = markers
        .as_slice()
        .iter()
        .map(|marker| {
            let source_dataset =
                ffi_string_to_option(marker.source_dataset_id, "marker overlay source_dataset_id")
                    .ok()
                    .flatten();
            let source_row_id =
                ffi_string_to_option(marker.source_row_id, "marker overlay source_row_id")
                    .ok()
                    .flatten();
            let source_row = match (source_dataset, source_row_id) {
                (Some(dataset_id), Some(row_id)) => Some((dataset_id, row_id)),
                _ => None,
            };
            MarkerOverlayItem {
                x: marker.x,
                y: marker.y,
                shape: match marker.shape {
                    FfiMarkerShape::Circle => MarkerShape::FilledCircle,
                    FfiMarkerShape::Square => MarkerShape::Box,
                    FfiMarkerShape::Point => MarkerShape::Point,
                    FfiMarkerShape::Cross => MarkerShape::Cross,
                    FfiMarkerShape::Box => MarkerShape::Box,
                    FfiMarkerShape::Ellipse => MarkerShape::Ellipse,
                    FfiMarkerShape::Diamond => MarkerShape::Diamond,
                    FfiMarkerShape::FilledCircle => MarkerShape::FilledCircle,
                },
                size: marker.size,
                color: marker.color.to_rgba(),
                timestamp_us: marker.has_timestamp.then_some(marker.timestamp_us),
                stable_id: ffi_string_to_option(marker.stable_id, "marker overlay stable_id")
                    .ok()
                    .flatten(),
                source_row,
            }
        })
        .collect();
    bridge.output.overlays.push(Overlay::MarkerOverlay {
        markers,
        dataset_id,
        layer_id,
        source_label,
    });
}

unsafe extern "C" fn add_warning(
    ctx: *mut c_void,
    source: FfiString,
    severity: AnalysisSeverity,
    message: FfiString,
) {
    let Some(bridge) = ctx.cast::<OutputBridge>().as_mut() else {
        return;
    };
    let Ok(source) = source.as_str() else {
        return;
    };
    let Ok(message) = message.as_str() else {
        return;
    };
    bridge.output.warnings.push(AnalysisWarning {
        source: source.to_owned(),
        severity: match severity {
            AnalysisSeverity::Info => CoreSeverity::Info,
            AnalysisSeverity::Warning => CoreSeverity::Warning,
            AnalysisSeverity::Error => CoreSeverity::Error,
        },
        message: message.to_owned(),
    });
}

unsafe extern "C" fn publish_context_value(ctx: *mut c_void, key: FfiString, data: FfiSlice<u8>) {
    let Some(bridge) = ctx.cast::<ContextBridge>().as_mut() else {
        return;
    };
    let Ok(key) = key.as_str() else {
        return;
    };
    bridge.data.insert(key.to_owned(), data.as_slice().to_vec());
}

unsafe extern "C" fn publish_persistent_context_value(
    ctx: *mut c_void,
    key: FfiString,
    data: FfiSlice<u8>,
) {
    let Some(bridge) = ctx.cast::<ContextBridge>().as_mut() else {
        return;
    };
    let Ok(key) = key.as_str() else {
        return;
    };
    bridge
        .persistent_data
        .insert(key.to_owned(), data.as_slice().to_vec());
}

unsafe extern "C" fn get_context_value(
    ctx: *mut c_void,
    key: FfiString,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> bool {
    if out_ptr.is_null() || out_len.is_null() {
        return false;
    }

    let found = (|| {
        let bridge = ctx.cast::<ContextBridge>().as_mut()?;
        let key = key.as_str().ok()?;
        bridge.data.get(key)
    })();

    match found {
        Some(bytes) => {
            *out_ptr = bytes.as_ptr();
            *out_len = bytes.len();
            true
        }
        None => {
            *out_ptr = std::ptr::null();
            *out_len = 0;
            false
        }
    }
}

unsafe extern "C" fn get_persistent_context_value(
    ctx: *mut c_void,
    key: FfiString,
    out_ptr: *mut *const u8,
    out_len: *mut usize,
) -> bool {
    if out_ptr.is_null() || out_len.is_null() {
        return false;
    }

    let found = (|| {
        let bridge = ctx.cast::<ContextBridge>().as_mut()?;
        let key = key.as_str().ok()?;
        bridge.persistent_data.get(key)
    })();

    match found {
        Some(bytes) => {
            *out_ptr = bytes.as_ptr();
            *out_len = bytes.len();
            true
        }
        None => {
            *out_ptr = std::ptr::null();
            *out_len = 0;
            false
        }
    }
}

unsafe extern "C" fn frame_at_callback(
    ctx: *const c_void,
    index: usize,
    out_frame: *mut FfiEventFrame,
) -> bool {
    let Some(slot) = out_frame.as_mut() else {
        return false;
    };
    let bridge = ctx.cast::<EventStoreBridge>().as_ref();
    let frame = bridge.and_then(|bridge| {
        let (window_start_us, window_end_us) = bridge.store.frame_window(index)?;
        // SAFETY: `frame_at_callback` is only called synchronously during
        // `process_frame`, and the host guarantees non-reentrancy.
        let materialized = unsafe { &mut *bridge.materialized_frames.get() };
        if let std::collections::hash_map::Entry::Vacant(entry) = materialized.entry(index) {
            entry.insert(bridge.store.materialize_frame(index)?);
        }
        let events = materialized.get(&index)?;
        Some(FfiEventFrame::from_slice(
            events,
            window_start_us,
            window_end_us,
        ))
    });
    match frame {
        Some(frame) => {
            *slot = frame;
            true
        }
        None => {
            *slot = FfiEventFrame::empty();
            false
        }
    }
}

unsafe extern "C" fn frame_range_for_timestamps_callback(
    ctx: *const c_void,
    start_us: u64,
    end_us: u64,
    out_start: *mut usize,
    out_end: *mut usize,
) -> bool {
    if out_start.is_null() || out_end.is_null() {
        return false;
    }

    let range = ctx
        .cast::<EventStoreBridge>()
        .as_ref()
        .and_then(|bridge| bridge.store.frame_range_for_timestamps(start_us, end_us));

    match range {
        Some((start, end)) => {
            *out_start = start;
            *out_end = end;
            true
        }
        None => {
            *out_start = 0;
            *out_end = 0;
            false
        }
    }
}

unsafe extern "C" fn oldest_timestamp_callback(ctx: *const c_void) -> u64 {
    ctx.cast::<EventStoreBridge>()
        .as_ref()
        .and_then(|bridge| bridge.store.oldest_timestamp_us())
        .unwrap_or(0)
}

unsafe extern "C" fn frame_count_callback(ctx: *const c_void) -> usize {
    ctx.cast::<EventStoreBridge>()
        .as_ref()
        .map(|bridge| bridge.store.frame_count())
        .unwrap_or(0)
}

fn ffi_string_to_option(ffi: FfiString, context: &str) -> Result<Option<String>, String> {
    let text = unsafe { ffi.as_str() }.map_err(|err| format!("{context}: {err}"))?;
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

unsafe fn bytes_from_out_ptr<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len)
    }
}

fn load_record(entry_dir: PathBuf) -> ManagedPlugin {
    let display_name = entry_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown Plugin".into());
    let manifest = match read_manifest(&entry_dir) {
        Ok(manifest) => Some(manifest),
        Err(err) => {
            return ManagedPlugin {
                spec: PluginSpec {
                    entry_dir,
                    manifest: None,
                    display_name,
                },
                plugin: None,
                load_error: Some(err),
            };
        }
    };

    let spec = PluginSpec {
        entry_dir,
        manifest,
        display_name,
    };
    match DynPlugin::load(&spec) {
        Ok(plugin) => ManagedPlugin {
            spec,
            plugin: Some(plugin),
            load_error: None,
        },
        Err(err) => ManagedPlugin {
            spec,
            plugin: None,
            load_error: Some(err),
        },
    }
}

fn read_manifest(entry_dir: &Path) -> Result<PluginManifest, String> {
    let manifest_path = entry_dir.join("plugin.toml");
    let text = fs::read_to_string(&manifest_path)
        .map_err(|err| format!("reading {} failed: {err}", manifest_path.display()))?;
    toml::from_str(&text)
        .map_err(|err| format!("parsing {} failed: {err}", manifest_path.display()))
}

fn find_library_file(entry_dir: &Path, manifest: &PluginManifest) -> Result<PathBuf, String> {
    if let Some(library) = manifest.library.as_deref() {
        for candidate in candidate_library_names(library) {
            let path = entry_dir.join(candidate);
            if path.is_file() {
                return Ok(path);
            }
        }
    }

    let extension = library_extension();
    let mut candidates = Vec::new();
    let entries = fs::read_dir(entry_dir).map_err(|err| {
        format!(
            "reading plugin directory {} failed: {err}",
            entry_dir.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("reading plugin file failed: {err}"))?;
        let is_file = entry.file_type().map(|ft| ft.is_file()).unwrap_or(false);
        if !is_file {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            candidates.push(path);
        }
    }

    match candidates.len() {
        0 => Err(format!(
            "no .{extension} library found in {}",
            entry_dir.display()
        )),
        1 => Ok(candidates.remove(0)),
        _ => Err(format!(
            "multiple .{extension} libraries found in {}; specify `library` in plugin.toml",
            entry_dir.display()
        )),
    }
}

fn candidate_library_names(base: &str) -> Vec<String> {
    let extension = library_extension();
    let mut names = Vec::new();
    if base.ends_with(extension) {
        names.push(base.to_owned());
    } else {
        names.push(format!("{base}.{extension}"));
    }
    if !base.starts_with("lib") {
        names.push(format!("lib{base}.{extension}"));
    }
    names
}

fn library_extension() -> &'static str {
    if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    }
}

fn default_plugins_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".augur")
        .join("plugins")
}

fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .or_else(|| {
                let drive = std::env::var_os("HOMEDRIVE")?;
                let path = std::env::var_os("HOMEPATH")?;
                Some(PathBuf::from(format!(
                    "{}{}",
                    drive.to_string_lossy(),
                    path.to_string_lossy()
                )))
            })
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

pub fn open_directory(path: &Path) -> Result<(), String> {
    let status = if cfg!(target_os = "macos") {
        Command::new("open").arg(path).status()
    } else if cfg!(target_os = "windows") {
        Command::new("explorer").arg(path).status()
    } else {
        Command::new("xdg-open").arg(path).status()
    }
    .map_err(|err| format!("opening {} failed: {err}", path.display()))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "opening {} failed with status {status}",
            path.display()
        ))
    }
}

pub fn plugin_phase_label(input: PluginInput) -> &'static str {
    match input {
        PluginInput::FrameOnly => "FrameOnly",
        PluginInput::RawEvents => "RawEvents",
        PluginInput::DerivedData => "DerivedData",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use augur_core::pipeline::CdEvent;
    use std::{
        ffi::c_void,
        fs,
        time::{Duration, SystemTime},
    };

    fn event(timestamp: u64, x: u16) -> FfiCdEvent {
        FfiCdEvent::new(x, 0, timestamp, 1)
    }

    fn retained_frame(
        events: &[FfiCdEvent],
        window_start_us: u64,
        window_end_us: u64,
    ) -> PreviewFrame {
        let source = LiveEventSource::default();
        let cd_events: Vec<_> = events
            .iter()
            .copied()
            .map(|event| CdEvent {
                x: event.x,
                y: event.y,
                timestamp: event.timestamp_us(),
                polarity: event.is_on(),
            })
            .collect();
        let event_range = source
            .append_cd_frame(&cd_events, window_start_us, window_end_us)
            .expect("test events fit in default live event source");

        PreviewFrame {
            width: 4,
            height: 4,
            pixels: vec![0; 16],
            pixels_on: vec![0; 16],
            pixels_off: vec![0; 16],
            cached_total_histogram: vec![0; 1],
            cached_signed_histogram: vec![0; 1],
            on_count: events.len() as u64,
            off_count: 0,
            events: None,
            event_range: Some(event_range),
            event_source: Some(source),
            window_start_us,
            window_end_us,
        }
    }

    fn empty_preview_frame() -> PreviewFrame {
        PreviewFrame {
            width: 4,
            height: 4,
            pixels: vec![0; 16],
            pixels_on: vec![0; 16],
            pixels_off: vec![0; 16],
            cached_total_histogram: vec![0; 1],
            cached_signed_histogram: vec![0; 1],
            on_count: 0,
            off_count: 0,
            events: None,
            event_range: None,
            event_source: None,
            window_start_us: 0,
            window_end_us: 1,
        }
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

    #[test]
    fn event_store_callbacks_expose_retained_history() {
        let mut store = PluginEventHistory::default();
        store.push_frame(&retained_frame(&[event(10, 1), event(20, 2)], 10, 20));
        store.push_frame(&retained_frame(&[event(30, 3)], 30, 30));
        let cache = EventHistoryMaterializationCache::default();
        let bridge = EventStoreBridge {
            store: &store,
            materialized_frames: &cache.frames,
        };
        let mut out = FfiEventFrame::empty();

        unsafe {
            frame_at_callback((&bridge as *const EventStoreBridge).cast(), 0, &mut out);
            frame_at_callback((&bridge as *const EventStoreBridge).cast(), 0, &mut out);
        }

        let slice = unsafe { out.as_slice() };
        assert_eq!(slice, &[event(10, 1), event(20, 2)]);
        assert_eq!(
            unsafe { (*cache.frames.get()).len() },
            1,
            "repeated frame_at calls for one index reuse the materialized cache"
        );
        assert_eq!(
            unsafe { oldest_timestamp_callback((&bridge as *const EventStoreBridge).cast()) },
            10
        );
        assert_eq!(
            unsafe { frame_count_callback((&bridge as *const EventStoreBridge).cast()) },
            2
        );
    }

    #[test]
    fn range_callback_filters_event_history() {
        let mut store = PluginEventHistory::default();
        store.push_frame(&retained_frame(&[event(10, 1), event(20, 2)], 10, 20));
        store.push_frame(&retained_frame(&[event(30, 3), event(40, 4)], 30, 40));
        let cache = EventHistoryMaterializationCache::default();
        let bridge = EventStoreBridge {
            store: &store,
            materialized_frames: &cache.frames,
        };
        let mut out_start = usize::MAX;
        let mut out_end = usize::MAX;

        unsafe {
            frame_range_for_timestamps_callback(
                (&bridge as *const EventStoreBridge).cast(),
                15,
                35,
                &mut out_start,
                &mut out_end,
            );
        }

        assert_eq!((out_start, out_end), (0, 2));
    }

    #[test]
    fn sync_from_upstream_captures_frames_without_preview_delivery() {
        let source = LiveEventSource::default();
        let cursor = source.register_cursor(
            "plugin:test",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );
        let first = [CdEvent {
            timestamp: 10,
            x: 1,
            y: 0,
            polarity: true,
        }];
        let second = [CdEvent {
            timestamp: 20,
            x: 2,
            y: 0,
            polarity: false,
        }];
        source
            .append_cd_frame(&first, 10, 10)
            .expect("first frame fits");
        source
            .append_cd_frame(&second, 20, 20)
            .expect("second frame fits");
        let mut store = PluginEventHistory::default();
        store.attach_upstream(source, Some(cursor));

        store.sync_from_upstream().expect("sync succeeds");

        assert_eq!(store.frame_count(), 2);
        assert_eq!(
            store.materialize_frame(0).as_deref(),
            Some(&[event(10, 1)][..])
        );
        assert_eq!(
            store.materialize_frame(1).as_deref(),
            Some(&[FfiCdEvent::new(2, 0, 20, 0)][..])
        );
    }

    #[test]
    fn synced_upstream_frames_survive_ring_eviction_after_cursor_advance() {
        let source = LiveEventSource::with_capacity(3);
        let cursor = source.register_cursor(
            "plugin:test",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );
        source
            .append_cd_frame(
                &[
                    CdEvent {
                        timestamp: 10,
                        x: 1,
                        y: 0,
                        polarity: true,
                    },
                    CdEvent {
                        timestamp: 20,
                        x: 2,
                        y: 0,
                        polarity: true,
                    },
                ],
                10,
                20,
            )
            .expect("seed frame fits");
        let mut store = PluginEventHistory::default();
        store.attach_upstream(source.clone(), Some(cursor));
        store.sync_from_upstream().expect("sync advances cursor");

        source
            .append_cd_frame(
                &[
                    CdEvent {
                        timestamp: 30,
                        x: 3,
                        y: 0,
                        polarity: true,
                    },
                    CdEvent {
                        timestamp: 40,
                        x: 4,
                        y: 0,
                        polarity: true,
                    },
                ],
                30,
                40,
            )
            .expect("advanced cursor permits eviction");

        assert_eq!(
            store.materialize_frame(0).as_deref(),
            Some(&[event(10, 1), event(20, 2)][..])
        );
    }

    #[test]
    fn missing_manifest_uses_directory_name_for_display() {
        let record = ManagedPlugin {
            spec: PluginSpec {
                entry_dir: PathBuf::from("/tmp/example-plugin"),
                manifest: None,
                display_name: "example-plugin".into(),
            },
            plugin: None,
            load_error: Some("manifest missing".into()),
        };

        assert_eq!(record.name(), "example-plugin");
        assert_eq!(record.description(), "");
        assert_eq!(record.domain(), "-");
        assert_eq!(record.version(), "-");
    }

    #[test]
    fn clearing_history_on_backward_seek_prevents_pre_seek_ranges_from_surviving() {
        let mut store = PluginEventHistory::default();
        store.push_frame(&retained_frame(&[event(100, 1)], 100, 100));
        store.push_frame(&retained_frame(&[event(200, 2)], 200, 200));

        store.clear();
        store.push_frame(&retained_frame(&[event(50, 3)], 50, 50));
        store.push_frame(&retained_frame(&[event(60, 4)], 60, 60));

        assert_eq!(store.frame_count(), 2);
        assert_eq!(store.frame_range_for_timestamps(0, 1_000), Some((0, 2)));
        assert_eq!(
            store.materialize_frame(0).as_deref(),
            Some(&[event(50, 3)][..])
        );
        assert_eq!(
            store.materialize_frame(1).as_deref(),
            Some(&[event(60, 4)][..])
        );
    }

    #[test]
    fn live_worker_drops_stale_epoch_jobs_but_publishes_current_jobs() {
        let plugins_dir = unique_temp_dir("worker-epochs");
        let (worker, rx) = LiveAnalysisWorker::spawn(plugins_dir.clone(), 1024 * 1024);
        worker.configure(
            2,
            LivePluginStateSnapshot::default(),
            PluginDiscontinuity::SettingsChanged,
        );
        worker.analyze(LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "stale epoch jobs must not publish results"
        );

        worker.analyze(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });
        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("current epoch job should publish a result");
        assert_eq!(result.epoch, 2);

        drop(worker);
        let _ = fs::remove_dir_all(plugins_dir);
    }

    #[test]
    fn clear_persistent_drops_worker_state_and_stale_echoes() {
        let plugins_dir = unique_temp_dir("worker-clear-persistent");
        let (worker, rx) = LiveAnalysisWorker::spawn(plugins_dir.clone(), 1024 * 1024);
        worker.configure(
            1,
            LivePluginStateSnapshot::default(),
            PluginDiscontinuity::SettingsChanged,
        );

        let mut seed = HashMap::new();
        seed.insert("plugin.state".to_owned(), b"old-source".to_vec());
        worker.analyze(LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: Some(seed),
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });
        let seeded = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("seeded job publishes");
        assert_eq!(
            seeded.persistent_data.get("plugin.state"),
            Some(&b"old-source".to_vec())
        );

        worker.clear_persistent(2);
        worker.analyze(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });
        let cleared = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("post-clear job publishes");
        assert!(
            cleared.persistent_data.is_empty(),
            "cleared persistent bus must not resurrect old-source values"
        );

        drop(worker);
        let _ = fs::remove_dir_all(plugins_dir);
    }

    #[test]
    fn coalesced_jobs_keep_superseded_persistent_updates() {
        let mut job = LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::from([("host.queue".to_owned(), Some(b"first".to_vec()))]),
            action_request_watermark: 3,
        };
        job.coalesce_with(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::from([("host.other".to_owned(), None)]),
            action_request_watermark: 2,
        });

        assert_eq!(job.epoch, 2);
        assert_eq!(job.action_request_watermark, 3);
        assert_eq!(
            job.persistent_updates.get("host.queue"),
            Some(&Some(b"first".to_vec())),
            "updates from superseded jobs must survive coalescing"
        );
        assert_eq!(job.persistent_updates.get("host.other"), Some(&None));

        let mut persistent = HashMap::new();
        job.apply_persistent_changes(&mut persistent);
        assert_eq!(persistent.get("host.queue"), Some(&b"first".to_vec()));
        assert!(!persistent.contains_key("host.other"));
    }

    #[test]
    fn seeded_coalesce_replaces_older_seed_and_updates() {
        let mut job = LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: Some(HashMap::from([("stale".to_owned(), b"a".to_vec())])),
            persistent_updates: HashMap::from([("stale.update".to_owned(), Some(b"b".to_vec()))]),
            action_request_watermark: 0,
        };
        job.coalesce_with(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            global_settings_json: None,
            persistent_seed: Some(HashMap::from([("fresh".to_owned(), b"c".to_vec())])),
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });

        let mut persistent = HashMap::from([("pre".to_owned(), b"x".to_vec())]);
        job.apply_persistent_changes(&mut persistent);
        assert_eq!(
            persistent,
            HashMap::from([("fresh".to_owned(), b"c".to_vec())]),
            "a newer full seed supersedes older seeds, updates, and worker state"
        );
    }

    #[test]
    fn snapshot_cache_skips_refetch_for_unchanged_nonzero_generations() {
        let mut cache = HostSnapshotCache::default();
        let key = (0usize, "table.demo".to_owned());
        let payload = || {
            Some(Ok(HostDatasetSnapshot::Series1d(Arc::new(
                augur_plugin_api::Series1dV1::default(),
            ))))
        };

        let mut fetches = 0;
        cache.resolve(key.clone(), 7, || {
            fetches += 1;
            payload()
        });
        cache.resolve(key.clone(), 7, || {
            fetches += 1;
            payload()
        });
        assert_eq!(fetches, 1, "unchanged generation must reuse the payload");

        cache.resolve(key.clone(), 8, || {
            fetches += 1;
            payload()
        });
        assert_eq!(fetches, 2, "a changed generation must refetch");

        let mut zero_fetches = 0;
        let zero_key = (0usize, "table.genless".to_owned());
        cache.resolve(zero_key.clone(), 0, || {
            zero_fetches += 1;
            payload()
        });
        cache.resolve(zero_key, 0, || {
            zero_fetches += 1;
            payload()
        });
        assert_eq!(
            zero_fetches, 2,
            "generation 0 means no counter and refetches every pass"
        );
    }

    #[test]
    fn decode_dataset_snapshot_validates_tables_against_schema() {
        let descriptor = HostDatasetDescriptor {
            id: "table.demo".into(),
            title: "Demo".into(),
            kind: HostDatasetKind::TableV1(augur_plugin_api::TableSchema {
                columns: vec![augur_plugin_api::TableColumn {
                    id: "x".into(),
                    title: "X".into(),
                    value_type: augur_plugin_api::TableValueType::F64,
                }],
                ..Default::default()
            }),
            empty_message: String::new(),
            display: None,
            relations: Vec::new(),
        };

        let valid = serde_json::json!({
            "columns": [{ "column_id": "x", "values": { "value_type": "f64", "values": [1.5] } }]
        });
        let decoded = decode_dataset_snapshot(&descriptor, &serde_json::to_vec(&valid).unwrap())
            .expect("valid table decodes");
        let HostDatasetSnapshot::Table(table) = decoded else {
            panic!("expected table payload");
        };
        assert_eq!(table.row_count(), 1);

        let wrong_type = serde_json::json!({
            "columns": [{ "column_id": "x", "values": { "value_type": "u64", "values": [1] } }]
        });
        let err = decode_dataset_snapshot(&descriptor, &serde_json::to_vec(&wrong_type).unwrap())
            .expect_err("schema mismatch must fail");
        assert!(err.contains("has type"));
    }

    #[test]
    fn detach_keeps_borrowed_cursors_registered() {
        let source = LiveEventSource::default();
        let controller_cursor = source.register_cursor(
            "plugin-runtime",
            CursorPolicy::Lossless {
                backpressure: BackpressureBehavior::FailLoud,
            },
        );

        let mut store = PluginEventHistory::default();
        store.attach_upstream(source.clone(), Some(controller_cursor));
        store.detach_upstream();

        assert!(
            source.drain_cursor_frames(controller_cursor).is_ok(),
            "borrowed controller cursor must survive detach so it can be re-attached"
        );

        store.attach_upstream(source.clone(), None);
        let owned_cursor = store.upstream_cursor.expect("owned cursor registered");
        store.detach_upstream();
        assert!(
            source.drain_cursor_frames(owned_cursor).is_err(),
            "self-registered cursors are unregistered on detach"
        );
    }

    unsafe extern "C" fn stub_create() -> *mut c_void {
        std::ptr::null_mut()
    }

    unsafe extern "C" fn stub_destroy(_: *mut c_void) {}

    unsafe extern "C" fn stub_string(_: *const c_void) -> FfiString {
        FfiString::default()
    }

    unsafe extern "C" fn stub_enabled(_: *const c_void) -> bool {
        false
    }

    unsafe extern "C" fn stub_set_enabled(_: *mut c_void, _: bool) {}

    unsafe extern "C" fn stub_reset(_: *mut c_void) {}

    unsafe extern "C" fn stub_on_discontinuity(_: *mut c_void, _: PluginDiscontinuity) {}

    unsafe extern "C" fn stub_input_kind(_: *const c_void) -> PluginInput {
        PluginInput::FrameOnly
    }

    unsafe extern "C" fn stub_plugin_state_kind(_: *const c_void) -> PluginStateKind {
        PluginStateKind::Accumulating
    }

    unsafe extern "C" fn stub_capabilities(_: *const c_void) -> PluginCapabilities {
        PluginCapabilities::default()
    }

    unsafe extern "C" fn stub_num_dependencies(_: *const c_void) -> usize {
        0
    }

    unsafe extern "C" fn stub_dependency(_: *const c_void, _: usize) -> FfiString {
        FfiString::default()
    }

    unsafe extern "C" fn stub_process_frame(
        _: *mut c_void,
        _: *const FfiPreviewFrame,
        _: *mut FfiOutputCallbacks,
        _: *mut FfiPluginContext,
        _: *const FfiEventStoreHandle,
    ) {
    }

    unsafe extern "C" fn stub_json(_: *const c_void, _: *mut *const u8, _: *mut usize) {}

    unsafe extern "C" fn stub_get_setting(
        _: *const c_void,
        _: FfiString,
        _: *mut *const u8,
        _: *mut usize,
    ) -> bool {
        false
    }

    unsafe extern "C" fn stub_set_setting(_: *mut c_void, _: FfiString, _: FfiSlice<u8>) -> bool {
        false
    }

    unsafe extern "C" fn stub_host_view_dataset(
        _: *const c_void,
        _: FfiString,
        _: *mut *const u8,
        _: *mut usize,
    ) -> bool {
        false
    }

    unsafe extern "C" fn stub_host_view_dataset_generation(_: *const c_void, _: FfiString) -> u64 {
        0
    }

    fn test_vtable() -> PluginVTable {
        PluginVTable {
            vtable_size: std::mem::size_of::<PluginVTable>(),
            abi_version: PLUGIN_ABI_VERSION,
            create: stub_create,
            destroy: stub_destroy,
            name: stub_string,
            description: stub_string,
            enabled: stub_enabled,
            set_enabled: stub_set_enabled,
            reset: stub_reset,
            on_discontinuity: stub_on_discontinuity,
            input_kind: stub_input_kind,
            capabilities: stub_capabilities,
            plugin_state_kind: stub_plugin_state_kind,
            num_dependencies: stub_num_dependencies,
            dependency: stub_dependency,
            process_frame: stub_process_frame,
            settings_schema: stub_json,
            get_setting: stub_get_setting,
            set_setting: stub_set_setting,
            status_entries: stub_json,
            host_views: stub_json,
            host_view_dataset: stub_host_view_dataset,
            host_view_dataset_generation: stub_host_view_dataset_generation,
        }
    }

    #[test]
    fn validate_plugin_vtable_rejects_abi_mismatches() {
        let mut vtable = test_vtable();
        vtable.abi_version = PLUGIN_ABI_VERSION - 1;

        let err = match validate_plugin_vtable(&vtable) {
            Ok(_) => panic!("abi mismatch should fail"),
            Err(err) => err,
        };
        assert!(err.contains("plugin ABI mismatch"));
    }

    #[test]
    fn validate_plugin_vtable_rejects_invalid_function_pointers() {
        let mut vtable = test_vtable();
        vtable.create =
            unsafe { std::mem::transmute::<usize, unsafe extern "C" fn() -> *mut c_void>(0xa0) };

        let err = match validate_plugin_vtable(&vtable) {
            Ok(_) => panic!("obviously invalid function pointer should fail"),
            Err(err) => err,
        };
        assert!(err.contains("invalid function pointers"));
    }
}
