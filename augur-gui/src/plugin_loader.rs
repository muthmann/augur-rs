use std::{
    collections::HashMap,
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use augur_core::{
    analysis::{
        AnalysisOutput, AnalysisSeverity as CoreSeverity, AnalysisWarning, MarkerOverlayItem,
        MarkerShape, Overlay, Pixel, SubpixelMarker,
    },
    pipeline::PreviewFrame,
};
use augur_plugin_api::{
    AnalysisSeverity, EventStore, FfiCdEvent, FfiColorRgba, FfiEventFrame, FfiEventStoreHandle,
    FfiMarkerOverlayItem, FfiMarkerShape, FfiOutputCallbacks, FfiPixel, FfiPluginContext,
    FfiPreviewFrame, FfiSlice, FfiString, FfiSubpixelMarker, HostViewRegistry, PluginCapabilities,
    PluginEntry, PluginInput, PluginVTable, SettingsSchema, StatusEntry, PLUGIN_ABI_VERSION,
    PLUGIN_ENTRY_SYMBOL,
};
use libloading::Library;
use serde::Deserialize;
use serde_json::Value;

pub const PLUGIN_UI_CACHE_INTERVAL: Duration = Duration::from_millis(250);
const MIN_PLAUSIBLE_FUNCTION_POINTER: usize = 4096;

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
        event_store: &EventStore,
        analysis_output: &mut AnalysisOutput,
        context_data: &mut HashMap<String, Vec<u8>>,
        persistent_data: &mut HashMap<String, Vec<u8>>,
    ) {
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
        let store_bridge = EventStoreBridge { store: event_store };
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

        unsafe {
            (self.vtable.process_frame)(
                self.instance,
                &ffi_frame,
                &mut callbacks,
                &mut plugin_context,
                &ffi_event_store,
            );
        }
        self.ui_dirty = false;
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
        vtable.input_kind as usize,
        vtable.capabilities as usize,
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

struct OutputBridge<'a> {
    output: &'a mut AnalysisOutput,
}

struct ContextBridge<'a> {
    data: &'a mut HashMap<String, Vec<u8>>,
    persistent_data: &'a mut HashMap<String, Vec<u8>>,
}

struct EventStoreBridge<'a> {
    store: &'a EventStore,
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
    let frame = ctx
        .cast::<EventStoreBridge>()
        .as_ref()
        .and_then(|bridge| bridge.store.frame(index));
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

fn open_directory(path: &Path) -> Result<(), String> {
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
    use std::ffi::c_void;

    fn event(timestamp: u64, x: u16) -> FfiCdEvent {
        FfiCdEvent {
            timestamp,
            x,
            y: 0,
            polarity: 1,
        }
    }

    #[test]
    fn event_store_callbacks_expose_retained_history() {
        let mut store = EventStore::default();
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3)], 30, 30);
        let bridge = EventStoreBridge { store: &store };
        let mut out = FfiEventFrame::empty();

        unsafe {
            frame_at_callback((&bridge as *const EventStoreBridge).cast(), 0, &mut out);
        }

        let slice = unsafe { out.as_slice() };
        assert_eq!(slice, &[event(10, 1), event(20, 2)]);
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
        let mut store = EventStore::default();
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3), event(40, 4)], 30, 40);
        let bridge = EventStoreBridge { store: &store };
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

    unsafe extern "C" fn stub_input_kind(_: *const c_void) -> PluginInput {
        PluginInput::FrameOnly
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
            input_kind: stub_input_kind,
            capabilities: stub_capabilities,
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
