use std::{
    cell::UnsafeCell,
    collections::{HashMap, HashSet, VecDeque},
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
    AnalysisSeverity, ExecutionContext, FfiCdEvent, FfiColorRgba, FfiEventFrame,
    FfiEventStoreHandle, FfiMarkerOverlayItem, FfiMarkerShape, FfiOutputCallbacks, FfiPixel,
    FfiPluginContext, FfiPluginControlContext, FfiPreviewFrame, FfiSlice, FfiString,
    FfiSubpixelMarker, HostCommand, HostCommandReply, HostCommandRequest, HostDatasetDescriptor,
    HostDatasetKind, HostViewRegistry, Image2dV1, PluginCapabilities, PluginControlInbox,
    PluginControlSnapshot, PluginDiscontinuity, PluginEntry, PluginInput, PluginRuntimeRole,
    PluginServiceOutcome, PluginServiceReply, PluginServiceRequest, PluginStateKind, PluginVTable,
    Series1dV1, SettingsSchema, StatusEntry, TableDatasetV1, CTX_SENSOR_MONITORING,
    PLUGIN_ABI_VERSION, PLUGIN_ENTRY_SYMBOL,
};
use libloading::Library;
use serde::Deserialize;
use serde_json::Value;

pub const PLUGIN_UI_CACHE_INTERVAL: Duration = Duration::from_millis(250);
const MIN_PLAUSIBLE_FUNCTION_POINTER: usize = 4096;
const DEFAULT_EVENT_HISTORY_BUDGET_BYTES: usize = 100 * 1024 * 1024;
const CONTROL_TICK_INTERVAL: Duration = Duration::from_millis(50);

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

/// Content-derived generation for providers that expose no counter. Never
/// returns `0`, which is reserved for "unknown".
fn content_generation(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    match hasher.finish() {
        0 => 1,
        other => other,
    }
}

impl HostSnapshotCache {
    /// Drops all cached payloads. Must be called whenever plugin instances
    /// are reconfigured, reloaded, or reset — a fresh instance may reuse
    /// generation numbers for different data.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Returns the *effective* generation and payload for `key`.
    ///
    /// A provider counter (`generation != 0`) short-circuits the fetch
    /// entirely. Providers without a counter are still fetched every pass —
    /// their bytes are the only way to tell whether anything changed — but the
    /// bytes are hashed into a stable effective generation, so an unchanged
    /// dataset skips JSON decoding here and, because the generation the GUI
    /// sees no longer flips to `0`, skips re-parsing downstream as well.
    fn resolve(
        &mut self,
        key: (usize, String),
        generation: u64,
        fetch_bytes: impl FnOnce() -> Result<Option<Vec<u8>>, String>,
        decode: impl FnOnce(&[u8]) -> Result<HostDatasetSnapshot, String>,
    ) -> (u64, Option<Result<HostDatasetSnapshot, String>>) {
        if generation != 0 {
            if let Some(cached) = self.entries.get(&key) {
                if cached.generation == generation {
                    return (generation, cached.payload.clone());
                }
            }
        }

        let (effective, payload) = match fetch_bytes() {
            Ok(Some(bytes)) => {
                let effective = if generation != 0 {
                    generation
                } else {
                    content_generation(&bytes)
                };
                if let Some(cached) = self.entries.get(&key) {
                    if cached.generation == effective {
                        return (effective, cached.payload.clone());
                    }
                }
                (effective, Some(decode(&bytes)))
            }
            Ok(None) => (generation, None),
            Err(err) => (generation, Some(Err(err))),
        };
        self.entries.insert(
            key,
            CachedHostSnapshot {
                generation: effective,
                payload: payload.clone(),
            },
        );
        (effective, payload)
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
    /// Worker-local monotonic order for host-view snapshots. Analysis and
    /// control results use separate channels, so the GUI must not infer
    /// freshness from receive order.
    pub host_snapshot_sequence: u64,
    pub host_snapshots: Vec<LivePluginHostSnapshot>,
    /// Highest host action request id that was visible on the persistent bus
    /// while this job ran. The GUI retires delivered requests up to this id.
    pub action_request_watermark: u64,
}

#[derive(Debug, Clone)]
pub struct RoutedHostCommandRequest {
    pub source_plugin_id: String,
    pub request: HostCommandRequest,
}

#[derive(Debug, Clone)]
pub struct LivePluginControlStatus {
    pub plugin_id: String,
    pub name: String,
    pub status_entries: Vec<StatusEntry>,
    pub snapshots: Vec<PluginControlSnapshot>,
}

/// Frame-independent worker state sent to the GUI. Host requests are
/// intentionally not executed in `augur-runtime`; the GUI remains the
/// capability arbiter and returns replies through `submit_host_reply`.
#[derive(Debug, Clone)]
pub struct LiveControlResult {
    pub epoch: u64,
    pub execution: ExecutionContext,
    pub plugins: Vec<LivePluginControlStatus>,
    /// Worker-local monotonic order for `host_snapshots` across both result
    /// channels.
    pub host_snapshot_sequence: u64,
    /// Frame-independent host-view state. Hardware-owning plugins can update
    /// datasets even when no camera preview frames are being analyzed.
    pub host_snapshots: Vec<LivePluginHostSnapshot>,
    pub host_requests: Vec<RoutedHostCommandRequest>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct LiveAnalysisWorker {
    tx: mpsc::Sender<LiveAnalysisCommand>,
    control_rx: mpsc::Receiver<LiveControlResult>,
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
        let (control_tx, control_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let join = thread::spawn(move || {
            run_live_analysis_worker(
                plugins_dir,
                memory_budget_bytes,
                rx,
                result_tx,
                control_tx,
                worker_stop,
            );
        });
        (
            Self {
                tx,
                control_rx,
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

    pub fn set_control_execution(&self, execution: ExecutionContext) {
        let (ack_tx, ack_rx) = mpsc::sync_channel(0);
        if self
            .tx
            .send(LiveAnalysisCommand::SetControlExecution {
                execution,
                ack: ack_tx,
            })
            .is_ok()
        {
            let _ = ack_rx.recv();
        }
    }

    pub fn submit_host_reply(&self, plugin_id: String, reply: HostCommandReply) {
        let _ = self
            .tx
            .send(LiveAnalysisCommand::HostReply { plugin_id, reply });
    }

    pub fn try_recv_control(&self) -> Result<LiveControlResult, mpsc::TryRecvError> {
        self.control_rx.try_recv()
    }
}

impl Drop for LiveAnalysisWorker {
    fn drop(&mut self) {
        // Revoke effects and give plugins one immediate control tick before
        // destroying worker-owned instances.
        self.set_control_execution(ExecutionContext::fail_closed());
        // The flag must be set *before* `Stop` is queued: it is the worker
        // loop's own guard, so storing it after `join()` would make the
        // `while !stop.load(..)` check dead and leave shutdown depending
        // entirely on the `Stop` message being reached.
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
    /// Where this job runs and whether hardware effects are permitted.
    /// Defaults to fail-closed (replay, no effects); the dispatching host
    /// sets `LiveCapture` + effects for the active live-capture worker only.
    pub execution: ExecutionContext,
    pub global_settings_json: Option<Vec<u8>>,
    /// Serialized `SensorMonitoringV1` for this frame, when the host is
    /// streaming from a camera that can measure it. `None` on replay and any
    /// source without a monitoring block.
    pub sensor_monitoring_json: Option<Vec<u8>>,
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
            execution,
            global_settings_json,
            sensor_monitoring_json,
            persistent_seed,
            persistent_updates,
            action_request_watermark,
        } = next;
        self.epoch = epoch;
        self.frame = frame;
        self.execution = execution;
        self.global_settings_json = global_settings_json;
        self.sensor_monitoring_json = sensor_monitoring_json;
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
    SetControlExecution {
        execution: ExecutionContext,
        ack: mpsc::SyncSender<()>,
    },
    HostReply {
        plugin_id: String,
        reply: HostCommandReply,
    },
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

        let replaces_other_source = self.upstream.is_some();
        self.detach_upstream();
        // Frames retained from the previous source describe a different
        // timeline: their windows carry that session's timestamps (a restarted
        // camera or a re-opened replay starts over), and their `Upstream`
        // payloads point at a ring nobody writes to any more. Keeping them
        // would interleave two timelines in `frames`.
        if replaces_other_source {
            self.clear();
        }
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
        self.push_retained_frame(PluginEventFrame {
            data,
            window_start_us: frame.window_start_us,
            window_end_us: frame.window_end_us,
            byte_len,
        });
    }

    fn push_upstream_batch(&mut self, batch: LiveEventFrameBatch) {
        if batch.events.is_empty() {
            return;
        }
        let byte_len = batch
            .events
            .len()
            .saturating_mul(std::mem::size_of::<FfiCdEvent>());
        self.push_retained_frame(PluginEventFrame {
            data: PluginEventFrameData::Inline(batch.events.into_boxed_slice()),
            window_start_us: batch.window_start_us,
            window_end_us: batch.window_end_us,
            byte_len,
        });
    }

    /// Appends a frame, keeping `frames` ordered by window.
    ///
    /// The ordering is a hard invariant: `frame_range_for_timestamps` binary
    /// searches over it and plugins walk the returned index range as a
    /// timeline. A frame whose window opens before the newest retained one
    /// belongs to a different timeline — a source switch, a backward seek, or
    /// a sensor timestamp reset — so the pre-jump history is dropped instead
    /// of being interleaved with it. Dropping unreachable history silently
    /// matches how the memory budget evicts frames.
    fn push_retained_frame(&mut self, frame: PluginEventFrame) {
        if self.frames.back().is_some_and(|newest| {
            frame.window_start_us < newest.window_start_us
                || frame.window_end_us < newest.window_end_us
        }) {
            self.clear();
        }
        let byte_len = frame.byte_len;
        self.frames.push_back(frame);
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

    /// Retained frames are ordered by window (see `push_retained_frame`), so
    /// the bounds can be binary searched. This runs on the plugin FFI callback
    /// path, where a panic would cross an `extern "C"` boundary and abort the
    /// process: the ordering is established on insertion, never asserted here.
    pub fn frame_range_for_timestamps(
        &self,
        start_timestamp_us: u64,
        end_timestamp_us: u64,
    ) -> Option<(usize, usize)> {
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
    /// Stable machine identity used for service routing and capability grants.
    /// Plugins that participate in the control plane must declare it.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub domain: Option<String>,
    pub library: Option<String>,
    /// Closed host-command verbs this plugin may emit. Declaration is only
    /// the first gate; the GUI applies its own persisted user consent.
    #[serde(default)]
    pub host_commands: Vec<String>,
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
    schema_fetched_at: Option<Instant>,
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
            schema_fetched_at: None,
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

    pub fn id(&self) -> Option<&str> {
        self.manifest.id.as_deref()
    }

    pub fn declared_host_commands(&self) -> &[String] {
        &self.manifest.host_commands
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

    /// Re-reads the settings schema from the plugin at the UI cache cadence.
    /// Plugins may build schemas from live state (e.g. the currently attached
    /// serial ports), so a schema cached only at load time goes stale.
    pub fn refresh_settings_schema_if_stale(&mut self) -> Result<(), String> {
        let now = Instant::now();
        if let Some(fetched_at) = self.schema_fetched_at {
            if now.duration_since(fetched_at) < PLUGIN_UI_CACHE_INTERVAL {
                return Ok(());
            }
        }
        self.schema_fetched_at = Some(now);
        let schema: SettingsSchema = self.read_json(self.vtable.settings_schema)?;
        if schema != self.cached_schema {
            self.cached_schema = schema;
        }
        Ok(())
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

    pub fn set_runtime_role(&mut self, role: PluginRuntimeRole) {
        unsafe { (self.vtable.set_runtime_role)(self.instance, role) };
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
            external_triggers: FfiSlice::from_slice(&frame.external_triggers),
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
            execution: pass.execution.as_ffi(),
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

    fn process_control(
        &mut self,
        inbox: &PluginControlInbox,
        execution: &ExecutionContext,
    ) -> Result<ControlEmissions, String> {
        let inbox_json = serde_json::to_vec(inbox)
            .map_err(|err| format!("serializing control inbox failed: {err}"))?;
        let mut emissions = ControlEmissions::default();
        let mut bridge = ControlOutputBridge {
            emissions: &mut emissions,
        };
        let mut context = FfiPluginControlContext {
            ctx: (&mut bridge as *mut ControlOutputBridge).cast(),
            inbox_json: FfiSlice::from_slice(&inbox_json),
            execution: execution.as_ffi(),
            emit_service_request,
            emit_host_command,
        };
        unsafe { (self.vtable.process_control)(self.instance, &mut context) };
        Ok(emissions)
    }

    fn handle_service_request(
        &mut self,
        request: &PluginServiceRequest,
        execution: &ExecutionContext,
    ) -> Result<PluginServiceReply, String> {
        let json = serde_json::to_vec(request)
            .map_err(|err| format!("serializing service request failed: {err}"))?;
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        unsafe {
            (self.vtable.handle_service_request)(
                self.instance,
                FfiSlice::from_slice(&json),
                execution.as_ffi(),
                &mut out_ptr,
                &mut out_len,
            );
        }
        let bytes = unsafe { bytes_from_out_ptr(out_ptr, out_len) };
        serde_json::from_slice(bytes)
            .map_err(|err| format!("plugin returned invalid service reply: {err}"))
    }

    fn control_snapshots(&self) -> Result<Vec<PluginControlSnapshot>, String> {
        self.read_json(self.vtable.control_snapshots)
    }
}

#[derive(Debug, Default)]
struct ControlEmissions {
    service_requests: Vec<PluginServiceRequest>,
    host_requests: Vec<HostCommandRequest>,
}

struct ControlOutputBridge<'a> {
    emissions: &'a mut ControlEmissions,
}

unsafe extern "C" fn emit_service_request(ctx: *mut c_void, data: FfiSlice<u8>) {
    let Some(bridge) = ctx.cast::<ControlOutputBridge>().as_mut() else {
        return;
    };
    if let Ok(request) = serde_json::from_slice(unsafe { data.as_slice() }) {
        bridge.emissions.service_requests.push(request);
    }
}

unsafe extern "C" fn emit_host_command(ctx: *mut c_void, data: FfiSlice<u8>) {
    let Some(bridge) = ctx.cast::<ControlOutputBridge>().as_mut() else {
        return;
    };
    if let Ok(request) = serde_json::from_slice(unsafe { data.as_slice() }) {
        bridge.emissions.host_requests.push(request);
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
        vtable.set_runtime_role as usize,
        vtable.reset as usize,
        vtable.on_discontinuity as usize,
        vtable.input_kind as usize,
        vtable.capabilities as usize,
        vtable.plugin_state_kind as usize,
        vtable.num_dependencies as usize,
        vtable.dependency as usize,
        vtable.process_frame as usize,
        vtable.process_control as usize,
        vtable.handle_service_request as usize,
        vtable.control_snapshots as usize,
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
    runtime_role: PluginRuntimeRole,
}

impl PluginManager {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self::with_runtime_role(plugins_dir, PluginRuntimeRole::UiMirror)
    }

    pub fn with_runtime_role(plugins_dir: PathBuf, runtime_role: PluginRuntimeRole) -> Self {
        Self {
            plugins_dir,
            records: Vec::new(),
            runtime_role,
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
            records.push(load_record(path, self.runtime_role));
        }

        let mut id_counts = HashMap::new();
        for id in records
            .iter()
            .filter_map(|record| record.plugin.as_ref().and_then(DynPlugin::id))
        {
            *id_counts.entry(id.to_owned()).or_insert(0_usize) += 1;
        }
        for record in &mut records {
            let duplicate_id = record
                .plugin
                .as_ref()
                .and_then(DynPlugin::id)
                .filter(|id| id_counts.get(*id).copied().unwrap_or_default() > 1)
                .map(str::to_owned);
            if let Some(id) = duplicate_id {
                record.plugin = None;
                record.load_error = Some(format!(
                    "duplicate stable plugin id `{id}`; every control-plane participant must have a unique manifest id"
                ));
            }
        }
        records.sort_by_key(|record| record.name().to_ascii_lowercase());
        self.records = records;
        Ok(())
    }

    pub fn reload_plugin(&mut self, index: usize) -> Result<(), String> {
        let Some(record) = self.records.get_mut(index) else {
            return Err(format!("plugin index {index} is out of range"));
        };
        let refreshed = load_record(record.spec.entry_dir.clone(), self.runtime_role);
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

/// Upper bound on retained request/reply dedupe entries, per map. Reached only
/// by a plugin issuing requests continuously for hours; eviction is
/// oldest-first, so a replayed *recent* request is still deduped.
const MAX_RETAINED_CONTROL_REQUESTS: usize = 1024;

#[derive(Default)]
struct ControlPlaneState {
    execution: ExecutionContext,
    service_replies: HashMap<String, Vec<PluginServiceReply>>,
    host_replies: HashMap<String, Vec<HostCommandReply>>,
    service_cache: HashMap<(String, u64), (PluginServiceRequest, PluginServiceReply)>,
    host_requests: HashMap<(String, u64), (HostCommandRequest, Option<HostCommandReply>)>,
    /// Insertion order for `service_cache` / `host_requests`, used to evict
    /// oldest-first once `MAX_RETAINED_CONTROL_REQUESTS` is exceeded.
    service_cache_order: VecDeque<(String, u64)>,
    host_requests_order: VecDeque<(String, u64)>,
}

impl ControlPlaneState {
    /// Drops every request/reply record while keeping `execution`.
    ///
    /// Must be called whenever plugin instances are recreated (reload) or
    /// reset (discontinuity): the dedupe maps are keyed by the plugin's own
    /// `request_id` counter, which restarts at 1 in a fresh instance. Without
    /// this, a recycled id either collides with a stale entry and is rejected
    /// as `request_id_conflict`, or — worse — matches one byte-for-byte and is
    /// answered from `service_cache` while the target plugin is never invoked,
    /// so a repeated `output_off.v1` reports `Accepted` without touching the
    /// device.
    fn reset_requests(&mut self) {
        self.service_replies.clear();
        self.host_replies.clear();
        self.service_cache.clear();
        self.host_requests.clear();
        self.service_cache_order.clear();
        self.host_requests_order.clear();
    }

    /// Retires settled dedupe entries while preserving requests still awaiting
    /// a host reply. Used at a discontinuity, where plugin instances survive
    /// (so an in-flight request is still owned by someone) but may restart
    /// their own request-id counters.
    fn forget_answered_requests(&mut self) {
        self.service_cache.clear();
        self.service_cache_order.clear();
        self.host_requests.retain(|_, (_, reply)| reply.is_none());
        self.host_requests_order
            .retain(|key| self.host_requests.contains_key(key));
    }

    fn cache_service_reply(
        &mut self,
        key: (String, u64),
        request: PluginServiceRequest,
        reply: PluginServiceReply,
    ) {
        if self
            .service_cache
            .insert(key.clone(), (request, reply))
            .is_none()
        {
            self.service_cache_order.push_back(key);
        }
        while self.service_cache_order.len() > MAX_RETAINED_CONTROL_REQUESTS {
            if let Some(oldest) = self.service_cache_order.pop_front() {
                self.service_cache.remove(&oldest);
            }
        }
    }

    fn record_host_request(&mut self, key: (String, u64), request: HostCommandRequest) {
        if self
            .host_requests
            .insert(key.clone(), (request, None))
            .is_none()
        {
            self.host_requests_order.push_back(key);
        }
        if self.host_requests_order.len() <= MAX_RETAINED_CONTROL_REQUESTS {
            return;
        }
        // Evict oldest-first, but never a request still awaiting its host
        // reply — dropping that would strand the plugin with no reply and no
        // timeout. In-flight requests are naturally few (each awaits a host
        // round-trip), so this always makes progress in practice.
        let mut excess = self.host_requests_order.len() - MAX_RETAINED_CONTROL_REQUESTS;
        let mut retained = VecDeque::with_capacity(self.host_requests_order.len());
        while let Some(key) = self.host_requests_order.pop_front() {
            let answered = matches!(self.host_requests.get(&key), Some((_, Some(_))));
            if excess > 0 && answered {
                self.host_requests.remove(&key);
                excess -= 1;
            } else {
                retained.push_back(key);
            }
        }
        self.host_requests_order = retained;
    }

    /// Fills an in-flight request's reply slot, ignoring later replies for the
    /// same id. Write-once matters because a plugin recording is finalized
    /// under its *start* request id when the operator (not the plugin) stopped
    /// it; overwriting would make a duplicate start emission replay
    /// `RecordingFinalized` instead of `RecordingStarted`.
    fn resolve_host_request(&mut self, plugin_id: String, reply: HostCommandReply) {
        let key = (plugin_id.clone(), reply.request_id);
        let Some((_, cached_reply)) = self.host_requests.get_mut(&key) else {
            return;
        };
        if cached_reply.is_none() {
            *cached_reply = Some(reply.clone());
        }
        self.host_replies.entry(plugin_id).or_default().push(reply);
    }

    /// Drops reply inboxes addressed to plugins that no longer exist or are
    /// disabled. Only enabled, id-bearing plugins drain their own inbox, so
    /// without this a disabled plugin's queue is retained for the whole
    /// process lifetime.
    fn prune_inboxes(&mut self, manager: &PluginManager) {
        let live: HashSet<&str> = manager
            .records()
            .iter()
            .filter_map(|record| record.plugin())
            .filter(|plugin| plugin.enabled())
            .filter_map(|plugin| plugin.id())
            .collect();
        self.service_replies
            .retain(|plugin_id, _| live.contains(plugin_id.as_str()));
        self.host_replies
            .retain(|plugin_id, _| live.contains(plugin_id.as_str()));
    }
}

fn host_command_verb(command: &HostCommand) -> &'static str {
    match command {
        HostCommand::StartRecording { .. } => "start_recording",
        HostCommand::StopRecording => "stop_recording",
    }
}

fn rejected_service_reply(
    request: &PluginServiceRequest,
    code: &str,
    message: String,
) -> PluginServiceReply {
    PluginServiceReply {
        request_id: request.request_id,
        source_plugin_id: request.source_plugin_id.clone(),
        target_plugin_id: request.target_plugin_id.clone(),
        service: request.service.clone(),
        outcome: PluginServiceOutcome::Rejected {
            code: code.into(),
            message,
        },
    }
}

fn collect_control_snapshots(
    manager: &PluginManager,
    warnings: &mut Vec<String>,
) -> Vec<PluginControlSnapshot> {
    let mut snapshots = Vec::new();
    for record in manager.records() {
        let Some(plugin) = record.plugin() else {
            continue;
        };
        if !plugin.enabled() {
            continue;
        }
        let Some(plugin_id) = plugin.id() else {
            continue;
        };
        match plugin.control_snapshots() {
            Ok(mut published) => {
                for snapshot in &mut published {
                    snapshot.plugin_id = plugin_id.to_owned();
                }
                snapshots.extend(published);
            }
            Err(err) => warnings.push(format!(
                "reading control snapshots from {plugin_id} failed: {err}"
            )),
        }
    }
    snapshots
}

fn tick_control_plane(
    manager: &mut PluginManager,
    state: &mut ControlPlaneState,
    host_snapshot_cache: &mut HostSnapshotCache,
    host_snapshot_sequence: &mut u64,
) -> LiveControlResult {
    let mut warnings = Vec::new();
    let snapshots = collect_control_snapshots(manager, &mut warnings);
    let mut service_requests = Vec::new();
    let mut routed_host_requests = Vec::new();

    for record in manager.records_mut() {
        let Some(plugin) = record.plugin_mut() else {
            continue;
        };
        if !plugin.enabled() {
            continue;
        }
        let Some(plugin_id) = plugin.id().map(str::to_owned) else {
            continue;
        };
        let inbox = PluginControlInbox {
            service_replies: state.service_replies.remove(&plugin_id).unwrap_or_default(),
            host_replies: state.host_replies.remove(&plugin_id).unwrap_or_default(),
            snapshots: snapshots.clone(),
        };
        match plugin.process_control(&inbox, &state.execution) {
            Ok(mut emissions) => {
                for request in &mut emissions.service_requests {
                    request.source_plugin_id = plugin_id.clone();
                }
                service_requests.extend(emissions.service_requests);
                for request in emissions.host_requests {
                    let verb = host_command_verb(&request.command);
                    if !plugin
                        .declared_host_commands()
                        .iter()
                        .any(|declared| declared == verb)
                    {
                        state
                            .host_replies
                            .entry(plugin_id.clone())
                            .or_default()
                            .push(HostCommandReply {
                                request_id: request.request_id,
                                outcome: augur_plugin_api::HostCommandOutcome::Rejected {
                                    code: "undeclared_host_command".into(),
                                    message: format!(
                                        "plugin manifest does not declare host command '{verb}'"
                                    ),
                                },
                            });
                        continue;
                    }
                    let key = (plugin_id.clone(), request.request_id);
                    if let Some((previous, reply)) = state.host_requests.get(&key) {
                        if previous != &request {
                            state
                                .host_replies
                                .entry(plugin_id.clone())
                                .or_default()
                                .push(HostCommandReply {
                                    request_id: request.request_id,
                                    outcome: augur_plugin_api::HostCommandOutcome::Rejected {
                                        code: "request_id_conflict".into(),
                                        message:
                                            "request id was reused for a different host command"
                                                .into(),
                                    },
                                });
                        } else if let Some(reply) = reply.clone() {
                            state
                                .host_replies
                                .entry(plugin_id.clone())
                                .or_default()
                                .push(reply);
                        }
                        continue;
                    }
                    state.record_host_request(key, request.clone());
                    routed_host_requests.push(RoutedHostCommandRequest {
                        source_plugin_id: plugin_id.clone(),
                        request,
                    });
                }
            }
            Err(err) => warnings.push(format!("control tick for {plugin_id} failed: {err}")),
        }
    }

    for request in service_requests {
        let key = (request.source_plugin_id.clone(), request.request_id);
        let reply = if let Some((previous, reply)) = state.service_cache.get(&key) {
            if previous == &request {
                reply.clone()
            } else {
                rejected_service_reply(
                    &request,
                    "request_id_conflict",
                    "request id was reused for a different service request".into(),
                )
            }
        } else {
            let target = manager
                .records_mut()
                .iter_mut()
                .filter_map(ManagedPlugin::plugin_mut)
                .find(|plugin| {
                    plugin.enabled() && plugin.id() == Some(request.target_plugin_id.as_str())
                });
            let reply = match target {
                Some(target) => target
                    .handle_service_request(&request, &state.execution)
                    .unwrap_or_else(|err| rejected_service_reply(&request, "target_error", err)),
                None => rejected_service_reply(
                    &request,
                    "target_unavailable",
                    format!(
                        "target plugin '{}' is not enabled",
                        request.target_plugin_id
                    ),
                ),
            };
            // Identities are host-owned even when a target returns malformed
            // or spoofed envelope fields.
            let reply = PluginServiceReply {
                request_id: request.request_id,
                source_plugin_id: request.source_plugin_id.clone(),
                target_plugin_id: request.target_plugin_id.clone(),
                service: request.service.clone(),
                outcome: reply.outcome,
            };
            state.cache_service_reply(key, request.clone(), reply.clone());
            reply
        };
        state
            .service_replies
            .entry(request.source_plugin_id.clone())
            .or_default()
            .push(reply);
    }
    state.prune_inboxes(manager);

    let snapshots = collect_control_snapshots(manager, &mut warnings);
    let plugins = manager
        .records()
        .iter()
        .filter_map(|record| {
            let plugin = record.plugin()?;
            let plugin_id = plugin.id()?.to_owned();
            if !plugin.enabled() {
                return None;
            }
            let status_entries = plugin.status_entries().unwrap_or_else(|err| {
                warnings.push(format!("reading status from {plugin_id} failed: {err}"));
                Vec::new()
            });
            Some(LivePluginControlStatus {
                snapshots: snapshots
                    .iter()
                    .filter(|snapshot| snapshot.plugin_id == plugin_id)
                    .cloned()
                    .collect(),
                plugin_id,
                name: plugin.name().to_owned(),
                status_entries,
            })
        })
        .collect();

    let (host_snapshot_sequence, host_snapshots) =
        collect_sequenced_live_host_snapshots(manager, host_snapshot_cache, host_snapshot_sequence);
    LiveControlResult {
        epoch: 0,
        execution: state.execution.clone(),
        plugins,
        host_snapshot_sequence,
        host_snapshots,
        host_requests: routed_host_requests,
        warnings,
    }
}

fn run_live_analysis_worker(
    plugins_dir: PathBuf,
    memory_budget_bytes: usize,
    rx: mpsc::Receiver<LiveAnalysisCommand>,
    result_tx: mpsc::Sender<LiveAnalysisResult>,
    control_tx: mpsc::Sender<LiveControlResult>,
    stop: Arc<AtomicBool>,
) {
    let mut state = WorkerState::new(plugins_dir, memory_budget_bytes);
    let mut last_control_tick = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let command = match rx.recv_timeout(CONTROL_TICK_INTERVAL) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let Some(command) = command else {
            if !state.publish_control(&control_tx) {
                break;
            }
            last_control_tick = Instant::now();
            continue;
        };
        let mut job = match state.handle_command(command, &control_tx, &mut last_control_tick) {
            WorkerStep::Handled { .. } => None,
            WorkerStep::Stop => break,
            WorkerStep::Analyze(job) => Some(job),
        };
        if let Some(job) = job.as_mut() {
            // Drain the queue so a burst of frames collapses into one analysis
            // pass, and so control commands queued behind them are not delayed
            // by a full frame period.
            let mut stopped = false;
            while let Ok(next) = rx.try_recv() {
                match state.handle_command(next, &control_tx, &mut last_control_tick) {
                    WorkerStep::Handled {
                        persistent_cleared: true,
                    } => {
                        // The pending job's merged seed/updates predate the
                        // clear and must not resurrect old values.
                        job.persistent_seed = None;
                        job.persistent_updates.clear();
                    }
                    WorkerStep::Handled { .. } => {}
                    WorkerStep::Stop => {
                        stopped = true;
                        break;
                    }
                    WorkerStep::Analyze(next_job) => job.coalesce_with(*next_job),
                }
            }
            if stopped {
                break;
            }
            if job.epoch >= state.active_epoch {
                state.active_epoch = state.active_epoch.max(job.epoch);
                job.apply_persistent_changes(&mut state.persistent_data);
                let mut result = process_live_analysis_job(
                    &mut state.manager,
                    &mut state.event_store,
                    &mut state.persistent_data,
                    &mut state.host_snapshot_cache,
                    &mut state.host_snapshot_sequence,
                    job,
                );
                if let Some(warning) = state.load_warning.take() {
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
        // A continuously populated analysis queue must not starve hardware
        // datasets or service/control progress. Publish after a command once
        // the same deadline used by the idle recv timeout has elapsed.
        if last_control_tick.elapsed() >= CONTROL_TICK_INTERVAL {
            if !state.publish_control(&control_tx) {
                break;
            }
            last_control_tick = Instant::now();
        }
    }
    // Revoke effects and give plugins one last control tick before the
    // worker-owned instances are dropped.
    state.control.execution = ExecutionContext::fail_closed();
    let _ = tick_control_plane(
        &mut state.manager,
        &mut state.control,
        &mut state.host_snapshot_cache,
        &mut state.host_snapshot_sequence,
    );
    state.event_store.detach_upstream();
}

/// Mutable state owned by the live-analysis worker thread.
struct WorkerState {
    manager: PluginManager,
    event_store: PluginEventHistory,
    persistent_data: HashMap<String, Vec<u8>>,
    host_snapshot_cache: HostSnapshotCache,
    host_snapshot_sequence: u64,
    active_epoch: u64,
    control: ControlPlaneState,
    load_warning: Option<String>,
}

/// What the worker loop must do once a command has been handled.
enum WorkerStep {
    /// Fully handled. `persistent_cleared` tells a job waiting behind this
    /// command to drop its now-stale persistent seed and updates.
    Handled {
        persistent_cleared: bool,
    },
    Stop,
    Analyze(Box<LiveAnalysisJob>),
}

impl WorkerState {
    fn new(plugins_dir: PathBuf, memory_budget_bytes: usize) -> Self {
        let mut manager =
            PluginManager::with_runtime_role(plugins_dir, PluginRuntimeRole::LiveWorker);
        let load_warning = manager.scan_and_load().err();
        let mut event_store = PluginEventHistory::default();
        event_store.set_memory_budget(memory_budget_bytes);
        Self {
            manager,
            event_store,
            persistent_data: HashMap::new(),
            host_snapshot_cache: HostSnapshotCache::default(),
            host_snapshot_sequence: 0,
            active_epoch: 0,
            control: ControlPlaneState::default(),
            load_warning,
        }
    }

    fn publish_control(&mut self, tx: &mpsc::Sender<LiveControlResult>) -> bool {
        let mut result = tick_control_plane(
            &mut self.manager,
            &mut self.control,
            &mut self.host_snapshot_cache,
            &mut self.host_snapshot_sequence,
        );
        result.epoch = self.active_epoch;
        tx.send(result).is_ok()
    }

    /// Handles one command. Shared by the idle `recv_timeout` path and the
    /// drain that runs ahead of a pending analysis job — the two used to carry
    /// byte-identical copies of every arm.
    fn handle_command(
        &mut self,
        command: LiveAnalysisCommand,
        control_tx: &mpsc::Sender<LiveControlResult>,
        last_control_tick: &mut Instant,
    ) -> WorkerStep {
        let handled = WorkerStep::Handled {
            persistent_cleared: false,
        };
        match command {
            LiveAnalysisCommand::Stop => WorkerStep::Stop,
            LiveAnalysisCommand::Analyze(job) => WorkerStep::Analyze(job),
            LiveAnalysisCommand::SetMemoryBudget(bytes) => {
                self.event_store.set_memory_budget(bytes);
                handled
            }
            LiveAnalysisCommand::SetControlExecution { execution, ack } => {
                self.control.execution = execution;
                if !self.publish_control(control_tx) {
                    return WorkerStep::Stop;
                }
                *last_control_tick = Instant::now();
                let _ = ack.send(());
                handled
            }
            LiveAnalysisCommand::HostReply { plugin_id, reply } => {
                self.control.resolve_host_request(plugin_id, reply);
                handled
            }
            LiveAnalysisCommand::Configure {
                epoch,
                snapshot,
                reason,
            } => {
                self.active_epoch = self.active_epoch.max(epoch);
                apply_live_plugin_snapshot(&mut self.manager, &snapshot);
                notify_plugins_of_discontinuity(&mut self.manager, reason);
                // Settings changes keep the same plugin instances, so in-flight
                // requests stay valid and must not be dropped.
                self.host_snapshot_cache.clear();
                self.event_store.clear();
                handled
            }
            LiveAnalysisCommand::Reload {
                epoch,
                snapshot,
                reason,
            } => {
                self.active_epoch = self.active_epoch.max(epoch);
                self.load_warning = self.manager.scan_and_load().err();
                apply_live_plugin_snapshot(&mut self.manager, &snapshot);
                notify_plugins_of_discontinuity(&mut self.manager, reason);
                self.host_snapshot_cache.clear();
                self.event_store.clear();
                // Instances were destroyed and recreated, so their request-id
                // counters restart at 1. Every dedupe entry is keyed by those
                // counters and would now alias a different request.
                self.control.reset_requests();
                handled
            }
            LiveAnalysisCommand::Discontinuity { epoch, reason } => {
                self.active_epoch = self.active_epoch.max(epoch);
                notify_plugins_of_discontinuity(&mut self.manager, reason);
                self.host_snapshot_cache.clear();
                self.event_store.clear();
                // Instances survive a discontinuity but may reset their own
                // counters, so retire settled entries. In-flight requests are
                // kept — dropping one would strand the plugin awaiting it.
                self.control.forget_answered_requests();
                handled
            }
            LiveAnalysisCommand::ClearPersistent { epoch } => {
                self.active_epoch = self.active_epoch.max(epoch);
                self.persistent_data.clear();
                WorkerStep::Handled {
                    persistent_cleared: true,
                }
            }
        }
    }
}

fn process_live_analysis_job(
    manager: &mut PluginManager,
    event_store: &mut PluginEventHistory,
    persistent_data: &mut HashMap<String, Vec<u8>>,
    host_snapshot_cache: &mut HostSnapshotCache,
    host_snapshot_sequence: &mut u64,
    job: &LiveAnalysisJob,
) -> LiveAnalysisResult {
    let mut output = AnalysisOutput::default();
    let mut context_data = HashMap::new();
    if let Some(json) = &job.global_settings_json {
        context_data.insert("augur.global_settings".to_owned(), json.clone());
    }
    if let Some(json) = &job.sensor_monitoring_json {
        context_data.insert(CTX_SENSOR_MONITORING.to_owned(), json.clone());
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
        execution: &job.execution,
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

    let (host_snapshot_sequence, host_snapshots) =
        collect_sequenced_live_host_snapshots(manager, host_snapshot_cache, host_snapshot_sequence);
    LiveAnalysisResult {
        epoch: job.epoch,
        output,
        context_data,
        persistent_data: persistent_data.clone(),
        host_snapshot_sequence,
        host_snapshots,
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
                            let (generation, payload) = cache.resolve(
                                (index, descriptor.id.clone()),
                                plugin.host_view_dataset_generation(&descriptor.id),
                                || plugin.host_view_dataset(&descriptor.id),
                                |bytes| decode_dataset_snapshot(descriptor, bytes),
                            );
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

fn collect_sequenced_live_host_snapshots(
    manager: &PluginManager,
    cache: &mut HostSnapshotCache,
    sequence: &mut u64,
) -> (u64, Vec<LivePluginHostSnapshot>) {
    *sequence = sequence.wrapping_add(1).max(1);
    (*sequence, collect_live_host_snapshots(manager, cache))
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
    /// Execution mode / effects gate for this pass, delivered to plugins
    /// through `HostContext::execution()`.
    pub execution: &'a ExecutionContext,
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

fn load_record(entry_dir: PathBuf, runtime_role: PluginRuntimeRole) -> ManagedPlugin {
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
        Ok(mut plugin) => {
            // Role assignment precedes enablement/settings mirroring, so a
            // GUI or offline instance can keep `set_setting` effect-free.
            plugin.set_runtime_role(runtime_role);
            ManagedPlugin {
                spec,
                plugin: Some(plugin),
                load_error: None,
            }
        }
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
    let manifest: PluginManifest = toml::from_str(&text)
        .map_err(|err| format!("parsing {} failed: {err}", manifest_path.display()))?;
    if let Some(id) = manifest.id.as_deref() {
        validate_plugin_id(id).map_err(|err| format!("{}: {err}", manifest_path.display()))?;
    }
    Ok(manifest)
}

fn validate_plugin_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return Err("plugin id must not be empty".into());
    };
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(format!(
            "plugin id `{id}` must start with a lowercase ASCII letter or digit"
        ));
    }
    if !chars
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_'))
    {
        return Err(format!(
            "plugin id `{id}` may contain only lowercase ASCII letters, digits, `.`, `-`, and `_`"
        ));
    }
    Ok(())
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
            external_triggers: Vec::new(),
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
            external_triggers: Vec::new(),
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
    fn manifest_accepts_stable_control_plane_identity_and_host_capabilities() {
        let plugins_dir = unique_temp_dir("manifest-control-id");
        fs::create_dir_all(&plugins_dir).expect("test plugin directory is created");
        fs::write(
            plugins_dir.join("plugin.toml"),
            r#"
id = "stage-a.modulation"
name = "Stage-A Modulation"
version = "1.0.0"
host_commands = ["start_recording", "stop_recording"]
"#,
        )
        .expect("manifest is written");

        let manifest = read_manifest(&plugins_dir).expect("manifest is valid");
        assert_eq!(manifest.id.as_deref(), Some("stage-a.modulation"));
        assert_eq!(
            manifest.host_commands,
            ["start_recording", "stop_recording"]
        );

        let _ = fs::remove_dir_all(plugins_dir);
    }

    #[test]
    fn manifest_rejects_ambiguous_control_plane_identity() {
        let plugins_dir = unique_temp_dir("manifest-invalid-id");
        fs::create_dir_all(&plugins_dir).expect("test plugin directory is created");
        fs::write(
            plugins_dir.join("plugin.toml"),
            "id = \"Stage A\"\nname = \"Stage A\"\nversion = \"1.0.0\"\n",
        )
        .expect("manifest is written");

        let error = read_manifest(&plugins_dir).expect_err("invalid id must fail");
        assert!(error.contains("must start with a lowercase ASCII letter or digit"));

        let _ = fs::remove_dir_all(plugins_dir);
    }

    #[test]
    fn backward_window_jump_drops_the_pre_jump_timeline() {
        let mut store = PluginEventHistory::default();
        store.push_frame(&retained_frame(&[event(100, 1)], 100, 100));
        store.push_frame(&retained_frame(&[event(200, 2)], 200, 200));

        // A restarted source replays low timestamps without an intervening
        // clear. Interleaving them would break the ordering the range queries
        // binary search over.
        store.push_frame(&retained_frame(&[event(50, 3)], 50, 50));

        assert_eq!(store.frame_count(), 1);
        assert_eq!(store.frame_window(0), Some((50, 50)));
        assert_eq!(store.frame_range_for_timestamps(0, 1_000), Some((0, 1)));
        assert_eq!(
            store.materialize_frame(0).as_deref(),
            Some(&[event(50, 3)][..])
        );
    }

    #[test]
    fn attaching_a_different_upstream_drops_the_previous_timeline() {
        let first = LiveEventSource::default();
        let second = LiveEventSource::default();

        let mut store = PluginEventHistory::default();
        store.attach_upstream(first, None);
        store.push_frame(&retained_frame(&[event(1_000, 1)], 1_000, 1_000));
        assert_eq!(store.frame_count(), 1);

        store.attach_upstream(second, None);

        assert_eq!(store.frame_count(), 0);
        assert_eq!(store.memory_usage_bytes(), 0);
        assert_eq!(store.frame_range_for_timestamps(0, 10_000), None);
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
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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
    fn live_worker_ticks_control_plane_without_analysis_frames() {
        let plugins_dir = unique_temp_dir("worker-control-tick");
        let (worker, _rx) = LiveAnalysisWorker::spawn(plugins_dir.clone(), 1024 * 1024);

        thread::sleep(CONTROL_TICK_INTERVAL.saturating_mul(2));
        let result = worker
            .try_recv_control()
            .expect("periodic control result arrives without an analysis frame");
        assert_eq!(result.execution, ExecutionContext::fail_closed());
        assert!(
            result.host_snapshot_sequence > 0,
            "frame-independent results carry an ordered host-view snapshot"
        );

        drop(worker);
        let _ = fs::remove_dir_all(plugins_dir);
    }

    #[test]
    fn control_execution_changes_are_acknowledged_before_returning() {
        let plugins_dir = unique_temp_dir("worker-control-revoke");
        let (worker, _rx) = LiveAnalysisWorker::spawn(plugins_dir.clone(), 1024 * 1024);
        let live = ExecutionContext {
            mode: augur_plugin_api::ExecutionMode::LiveCapture,
            effects_allowed: true,
            session_id: Some("test".into()),
        };

        worker.set_control_execution(live.clone());
        let mut observed = Vec::new();
        while let Ok(result) = worker.try_recv_control() {
            observed.push(result.execution);
        }
        assert!(observed.contains(&live));

        worker.set_control_execution(ExecutionContext::fail_closed());
        let mut observed = Vec::new();
        while let Ok(result) = worker.try_recv_control() {
            observed.push(result.execution);
        }
        assert!(observed.contains(&ExecutionContext::fail_closed()));

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
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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
    fn coalesced_jobs_take_the_newest_sensor_monitoring_reading() {
        let mut job = LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: Some(b"older-reading".to_vec()),
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        };
        job.coalesce_with(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: Some(b"newer-reading".to_vec()),
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });

        assert_eq!(
            job.sensor_monitoring_json.as_deref(),
            Some(b"newer-reading".as_slice()),
            "a coalesced job must carry the freshest measurement, not the one it superseded",
        );

        // A host that stopped reporting (panel closed, camera gone) must clear
        // the value rather than pin the last reading onto every later frame.
        job.coalesce_with(LiveAnalysisJob {
            epoch: 3,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::new(),
            action_request_watermark: 0,
        });
        assert_eq!(job.sensor_monitoring_json, None);
    }

    #[test]
    fn coalesced_jobs_keep_superseded_persistent_updates() {
        let mut job = LiveAnalysisJob {
            epoch: 1,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
            persistent_seed: None,
            persistent_updates: HashMap::from([("host.queue".to_owned(), Some(b"first".to_vec()))]),
            action_request_watermark: 3,
        };
        job.coalesce_with(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
            persistent_seed: Some(HashMap::from([("stale".to_owned(), b"a".to_vec())])),
            persistent_updates: HashMap::from([("stale.update".to_owned(), Some(b"b".to_vec()))]),
            action_request_watermark: 0,
        };
        job.coalesce_with(LiveAnalysisJob {
            epoch: 2,
            frame: empty_preview_frame(),
            execution: ExecutionContext::fail_closed(),
            global_settings_json: None,
            sensor_monitoring_json: None,
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

    fn decoded_series() -> Result<HostDatasetSnapshot, String> {
        Ok(HostDatasetSnapshot::Series1d(Arc::new(
            augur_plugin_api::Series1dV1::default(),
        )))
    }

    #[test]
    fn snapshot_cache_skips_refetch_for_unchanged_nonzero_generations() {
        let mut cache = HostSnapshotCache::default();
        let key = (0usize, "table.demo".to_owned());

        let mut fetches = 0;
        let fetch_once = |cache: &mut HostSnapshotCache, generation: u64, fetches: &mut i32| {
            cache.resolve(
                key.clone(),
                generation,
                || {
                    *fetches += 1;
                    Ok(Some(b"{}".to_vec()))
                },
                |_| decoded_series(),
            )
        };

        fetch_once(&mut cache, 7, &mut fetches);
        fetch_once(&mut cache, 7, &mut fetches);
        assert_eq!(fetches, 1, "unchanged generation must reuse the payload");

        fetch_once(&mut cache, 8, &mut fetches);
        assert_eq!(fetches, 2, "a changed generation must refetch");
    }

    #[test]
    fn snapshot_cache_derives_a_stable_generation_from_content() {
        let mut cache = HostSnapshotCache::default();
        let key = (0usize, "table.genless".to_owned());

        let mut decodes = 0;
        let resolve = |cache: &mut HostSnapshotCache, bytes: &'static [u8], decodes: &mut i32| {
            cache.resolve(
                key.clone(),
                0,
                || Ok(Some(bytes.to_vec())),
                |_| {
                    *decodes += 1;
                    decoded_series()
                },
            )
        };

        let (first, _) = resolve(&mut cache, b"{\"a\":1}", &mut decodes);
        let (second, _) = resolve(&mut cache, b"{\"a\":1}", &mut decodes);
        assert_ne!(
            first, 0,
            "a provider without a counter still reports a nonzero generation"
        );
        assert_eq!(
            first, second,
            "identical bytes must map to the same generation so consumers can cache"
        );
        assert_eq!(decodes, 1, "unchanged bytes must skip the JSON decode");

        let (third, _) = resolve(&mut cache, b"{\"a\":2}", &mut decodes);
        assert_ne!(second, third, "changed bytes must change the generation");
        assert_eq!(decodes, 2, "changed bytes must be decoded");
    }

    #[test]
    fn reset_requests_lets_a_recycled_request_id_reach_the_target_again() {
        let mut state = ControlPlaneState::default();
        let key = ("workflow".to_owned(), 1u64);
        let request = HostCommandRequest {
            request_id: 1,
            command: HostCommand::StopRecording,
        };
        state.record_host_request(key.clone(), request.clone());
        state.resolve_host_request(
            "workflow".to_owned(),
            HostCommandReply {
                request_id: 1,
                outcome: augur_plugin_api::HostCommandOutcome::Rejected {
                    code: "no_plugin_recording".into(),
                    message: String::new(),
                },
            },
        );
        assert!(state.host_requests.contains_key(&key));

        // A reload recreates the plugin, whose counter restarts at 1.
        state.reset_requests();
        assert!(
            !state.host_requests.contains_key(&key),
            "a recycled request id must not alias the previous instance's entry"
        );
    }

    #[test]
    fn host_reply_cache_is_write_once() {
        let mut state = ControlPlaneState::default();
        let key = ("workflow".to_owned(), 1u64);
        state.record_host_request(
            key.clone(),
            HostCommandRequest {
                request_id: 1,
                command: HostCommand::StartRecording {
                    run_id: "run-1".into(),
                    base_path: "/tmp".into(),
                    metadata: Default::default(),
                },
            },
        );
        let started = HostCommandReply {
            request_id: 1,
            outcome: augur_plugin_api::HostCommandOutcome::RecordingStarted {
                actual_raw_path: "/tmp/run-1.raw".into(),
                started_at: "2026-07-24T00:00:00Z".into(),
            },
        };
        state.resolve_host_request("workflow".to_owned(), started);
        // The operator stopped the run, so finalization is delivered under the
        // *start* id; it must not displace the cached start outcome.
        state.resolve_host_request(
            "workflow".to_owned(),
            HostCommandReply {
                request_id: 1,
                outcome: augur_plugin_api::HostCommandOutcome::Rejected {
                    code: "later".into(),
                    message: String::new(),
                },
            },
        );
        let (_, cached) = &state.host_requests[&key];
        assert!(
            matches!(
                cached,
                Some(HostCommandReply {
                    outcome: augur_plugin_api::HostCommandOutcome::RecordingStarted { .. },
                    ..
                })
            ),
            "the first reply wins so a duplicate emission replays the start outcome"
        );
        assert_eq!(
            state.host_replies["workflow"].len(),
            2,
            "both replies are still delivered to the plugin inbox"
        );
    }

    #[test]
    fn answered_host_requests_are_evicted_but_in_flight_ones_survive() {
        let mut state = ControlPlaneState::default();
        let in_flight = ("workflow".to_owned(), 0u64);
        state.record_host_request(
            in_flight.clone(),
            HostCommandRequest {
                request_id: 0,
                command: HostCommand::StopRecording,
            },
        );
        for request_id in 1..=(MAX_RETAINED_CONTROL_REQUESTS as u64 + 64) {
            let key = ("workflow".to_owned(), request_id);
            state.record_host_request(
                key,
                HostCommandRequest {
                    request_id,
                    command: HostCommand::StopRecording,
                },
            );
            state.resolve_host_request(
                "workflow".to_owned(),
                HostCommandReply {
                    request_id,
                    outcome: augur_plugin_api::HostCommandOutcome::Rejected {
                        code: "no_plugin_recording".into(),
                        message: String::new(),
                    },
                },
            );
        }
        assert!(
            state.host_requests.len() <= MAX_RETAINED_CONTROL_REQUESTS + 1,
            "answered entries must be evicted, got {}",
            state.host_requests.len()
        );
        assert!(
            state.host_requests.contains_key(&in_flight),
            "a request still awaiting its reply must never be evicted"
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

    unsafe extern "C" fn stub_set_runtime_role(_: *mut c_void, _: PluginRuntimeRole) {}

    unsafe extern "C" fn stub_process_control(_: *mut c_void, _: *mut FfiPluginControlContext) {}

    unsafe extern "C" fn stub_handle_service_request(
        _: *mut c_void,
        _: FfiSlice<u8>,
        _: augur_plugin_api::FfiExecutionContext,
        _: *mut *const u8,
        _: *mut usize,
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
            set_runtime_role: stub_set_runtime_role,
            reset: stub_reset,
            on_discontinuity: stub_on_discontinuity,
            input_kind: stub_input_kind,
            capabilities: stub_capabilities,
            plugin_state_kind: stub_plugin_state_kind,
            num_dependencies: stub_num_dependencies,
            dependency: stub_dependency,
            process_frame: stub_process_frame,
            process_control: stub_process_control,
            handle_service_request: stub_handle_service_request,
            control_snapshots: stub_json,
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
