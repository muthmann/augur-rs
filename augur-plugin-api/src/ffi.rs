use std::{ffi::c_void, str::Utf8Error};

use serde::{Deserialize, Serialize};

pub type FfiCdEvent = augur_event_types::CompactEvent;
pub type FfiExternalTriggerEvent = augur_event_types::ExternalTriggerEvent;

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiSlice<T> {
    pub ptr: *const T,
    pub len: usize,
}

impl<T> Default for FfiSlice<T> {
    fn default() -> Self {
        Self::empty()
    }
}

impl<T> FfiSlice<T> {
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    pub fn from_slice(slice: &[T]) -> Self {
        Self {
            ptr: slice.as_ptr(),
            len: slice.len(),
        }
    }

    /// Returns a borrowed view over the raw FFI buffer.
    ///
    /// # Safety
    /// `self.ptr` must be null or point to `self.len` contiguous initialized values of `T`
    /// that remain valid for the returned lifetime. The pointed-to memory must not be mutated
    /// for the duration of the returned slice.
    pub unsafe fn as_slice<'a>(&self) -> &'a [T] {
        if self.ptr.is_null() || self.len == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.ptr, self.len)
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiString {
    pub ptr: *const u8,
    pub len: usize,
}

impl Default for FfiString {
    fn default() -> Self {
        Self::empty()
    }
}

impl FfiString {
    pub const fn empty() -> Self {
        Self {
            ptr: std::ptr::null(),
            len: 0,
        }
    }

    pub fn borrowed(value: &str) -> Self {
        Self {
            ptr: value.as_ptr(),
            len: value.len(),
        }
    }

    /// Returns a borrowed UTF-8 view over the raw FFI buffer.
    ///
    /// # Safety
    /// `self.ptr` must be null or point to `self.len` bytes that remain valid for the returned
    /// lifetime, and the pointed-to bytes must contain valid UTF-8.
    pub unsafe fn as_str<'a>(&self) -> Result<&'a str, Utf8Error> {
        if self.ptr.is_null() || self.len == 0 {
            Ok("")
        } else {
            std::str::from_utf8(std::slice::from_raw_parts(self.ptr, self.len))
        }
    }
}

impl From<&str> for FfiString {
    fn from(value: &str) -> Self {
        Self::borrowed(value)
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginInput {
    FrameOnly = 0,
    RawEvents = 1,
    DerivedData = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginStateKind {
    #[default]
    Accumulating = 0,
    Stateless = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginDiscontinuity {
    Seek = 0,
    SourceChanged = 1,
    HistoryEvicted = 2,
    SettingsChanged = 3,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCapabilities {
    pub retained_event_history: bool,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisSeverity {
    Info = 0,
    Warning = 1,
    Error = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiPixel {
    pub x: u16,
    pub y: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FfiSubpixelMarker {
    pub x: f32,
    pub y: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfiMarkerShape {
    Circle = 0,
    Square = 1,
    Point = 2,
    Cross = 3,
    Box = 4,
    Ellipse = 5,
    Diamond = 6,
    FilledCircle = 7,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FfiColorRgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl FfiColorRgba {
    pub const fn from_rgba(rgba: [u8; 4]) -> Self {
        Self {
            r: rgba[0],
            g: rgba[1],
            b: rgba[2],
            a: rgba[3],
        }
    }

    pub const fn to_rgba(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FfiMarkerOverlayItem {
    pub x: f32,
    pub y: f32,
    pub shape: FfiMarkerShape,
    pub size: f32,
    pub color: FfiColorRgba,
    pub timestamp_us: u64,
    pub has_timestamp: bool,
    pub stable_id: FfiString,
    /// Optional pointer to the row that backs this marker. When set, a
    /// click on the marker selects `(source_dataset_id, source_row_id)`
    /// instead of falling back to `(overlay.dataset_id, stable_id)`.
    /// Empty `FfiString`s mean "no explicit source row".
    pub source_dataset_id: FfiString,
    pub source_row_id: FfiString,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FfiEventFrame {
    pub events: FfiSlice<FfiCdEvent>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

impl FfiEventFrame {
    pub const fn empty() -> Self {
        Self {
            events: FfiSlice::empty(),
            window_start_us: 0,
            window_end_us: 0,
        }
    }

    pub fn from_slice(events: &[FfiCdEvent], window_start_us: u64, window_end_us: u64) -> Self {
        Self {
            events: FfiSlice::from_slice(events),
            window_start_us,
            window_end_us,
        }
    }

    /// Returns the raw event slice carried by this frame.
    ///
    /// # Safety
    /// The underlying pointer must remain valid for the duration of the returned borrow.
    pub unsafe fn as_slice<'a>(&self) -> &'a [FfiCdEvent] {
        unsafe { self.events.as_slice() }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FfiPreviewFrame {
    pub width: u16,
    pub height: u16,
    pub pixels: FfiSlice<u16>,
    pub events: FfiSlice<FfiCdEvent>,
    /// EVT3 `EXT_TRIGGER` edges inside this frame's window, camera-clock
    /// timestamps, in timestamp order.
    pub external_triggers: FfiSlice<FfiExternalTriggerEvent>,
    pub window_start_us: u64,
    pub window_end_us: u64,
}

/// Where a plugin invocation is running and whether hardware side effects
/// are permitted. Plugins that talk to lab instruments must fail closed
/// unless `mode == LiveCapture` **and** `effects_allowed != 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    LiveCapture = 0,
    Replay = 1,
    OfflineAnalysis = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FfiExecutionContext {
    pub mode: ExecutionMode,
    /// Boolean as a fixed-layout byte: nonzero = effects allowed.
    pub effects_allowed: u8,
    pub _reserved: [u8; 7],
    /// Optional host-assigned session identifier (empty = none).
    pub session_id: FfiString,
}

impl FfiExecutionContext {
    /// The most restrictive context: replay semantics, no effects.
    pub const fn fail_closed() -> Self {
        Self {
            mode: ExecutionMode::Replay,
            effects_allowed: 0,
            _reserved: [0; 7],
            session_id: FfiString::empty(),
        }
    }
}

pub type AddHighlightPixelsFn = unsafe extern "C" fn(*mut c_void, FfiSlice<FfiPixel>, FfiColorRgba);
pub type AddCrosshairMarkersFn =
    unsafe extern "C" fn(*mut c_void, FfiSlice<FfiSubpixelMarker>, FfiColorRgba, u16);
pub type AddMarkerOverlayFn = unsafe extern "C" fn(
    *mut c_void,
    FfiSlice<FfiMarkerOverlayItem>,
    FfiString,
    FfiString,
    FfiString,
);
pub type AddWarningFn = unsafe extern "C" fn(*mut c_void, FfiString, AnalysisSeverity, FfiString);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiOutputCallbacks {
    pub ctx: *mut c_void,
    pub add_highlight_pixels: AddHighlightPixelsFn,
    pub add_crosshair_markers: AddCrosshairMarkersFn,
    pub add_marker_overlay: AddMarkerOverlayFn,
    pub add_warning: AddWarningFn,
}

pub type ContextPublishFn = unsafe extern "C" fn(*mut c_void, FfiString, FfiSlice<u8>);
pub type ContextGetFn =
    unsafe extern "C" fn(*mut c_void, FfiString, *mut *const u8, *mut usize) -> bool;
pub type EventStoreFrameAtFn =
    unsafe extern "C" fn(*const c_void, usize, *mut FfiEventFrame) -> bool;
pub type EventStoreFrameRangeForTimestampsFn =
    unsafe extern "C" fn(*const c_void, u64, u64, *mut usize, *mut usize) -> bool;
pub type EventStoreOldestTsFn = unsafe extern "C" fn(*const c_void) -> u64;
pub type EventStoreFrameCountFn = unsafe extern "C" fn(*const c_void) -> usize;
pub type HostViewDatasetGenerationFn = unsafe extern "C" fn(*const c_void, FfiString) -> u64;
pub type PluginCapabilitiesFn = unsafe extern "C" fn(*const c_void) -> PluginCapabilities;
pub type PluginStateKindFn = unsafe extern "C" fn(*const c_void) -> PluginStateKind;
pub type PluginDiscontinuityFn = unsafe extern "C" fn(*mut c_void, PluginDiscontinuity);

pub const PLUGIN_ABI_VERSION: u64 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiEventStoreHandle {
    pub ctx: *const c_void,
    pub frame_count: EventStoreFrameCountFn,
    pub frame_at: EventStoreFrameAtFn,
    pub frame_range_for_timestamps: EventStoreFrameRangeForTimestampsFn,
    pub oldest_timestamp_us: EventStoreOldestTsFn,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FfiPluginContext {
    pub ctx: *mut c_void,
    pub raw_events: FfiSlice<FfiCdEvent>,
    pub publish: ContextPublishFn,
    pub get: ContextGetFn,
    pub publish_persistent: ContextPublishFn,
    pub get_persistent: ContextGetFn,
    pub execution: FfiExecutionContext,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PluginVTable {
    /// Size of this struct in bytes. The host uses this to detect plugins
    /// compiled against an older (smaller) API before copying the vtable.
    pub vtable_size: usize,
    /// Version of the runtime plugin ABI layout.
    ///
    /// This guards against stale plugins whose `PluginVTable` happens to keep the
    /// same total size even though fields were inserted or reordered.
    pub abi_version: u64,
    pub create: unsafe extern "C" fn() -> *mut c_void,
    pub destroy: unsafe extern "C" fn(*mut c_void),
    pub name: unsafe extern "C" fn(*const c_void) -> FfiString,
    pub description: unsafe extern "C" fn(*const c_void) -> FfiString,
    pub enabled: unsafe extern "C" fn(*const c_void) -> bool,
    pub set_enabled: unsafe extern "C" fn(*mut c_void, bool),
    pub reset: unsafe extern "C" fn(*mut c_void),
    pub on_discontinuity: PluginDiscontinuityFn,
    pub input_kind: unsafe extern "C" fn(*const c_void) -> PluginInput,
    pub capabilities: PluginCapabilitiesFn,
    pub plugin_state_kind: PluginStateKindFn,
    pub num_dependencies: unsafe extern "C" fn(*const c_void) -> usize,
    pub dependency: unsafe extern "C" fn(*const c_void, usize) -> FfiString,
    pub process_frame: unsafe extern "C" fn(
        *mut c_void,
        *const FfiPreviewFrame,
        *mut FfiOutputCallbacks,
        *mut FfiPluginContext,
        *const FfiEventStoreHandle,
    ),
    pub settings_schema: unsafe extern "C" fn(*const c_void, *mut *const u8, *mut usize),
    pub get_setting:
        unsafe extern "C" fn(*const c_void, FfiString, *mut *const u8, *mut usize) -> bool,
    pub set_setting: unsafe extern "C" fn(*mut c_void, FfiString, FfiSlice<u8>) -> bool,
    pub status_entries: unsafe extern "C" fn(*const c_void, *mut *const u8, *mut usize),
    pub host_views: unsafe extern "C" fn(*const c_void, *mut *const u8, *mut usize),
    pub host_view_dataset:
        unsafe extern "C" fn(*const c_void, FfiString, *mut *const u8, *mut usize) -> bool,
    pub host_view_dataset_generation: HostViewDatasetGenerationFn,
}

pub type PluginEntry = unsafe extern "C" fn() -> *const PluginVTable;

pub const PLUGIN_ENTRY_SYMBOL: &str = "augur_plugin_vtable";
