use serde::{de::DeserializeOwned, Serialize};

use crate::{
    ffi::{
        AnalysisSeverity, FfiCdEvent, FfiColorRgba, FfiEventFrame, FfiEventStoreHandle,
        FfiMarkerOverlayItem, FfiOutputCallbacks, FfiPixel, FfiPluginContext, FfiPreviewFrame,
        FfiSlice, FfiString, FfiSubpixelMarker, PluginCapabilities, PluginInput,
    },
    settings::{SettingsSchema, StatusEntry},
    HostViewRegistry,
};

pub struct PluginFrame<'a> {
    raw: &'a FfiPreviewFrame,
}

impl<'a> PluginFrame<'a> {
    pub fn new(raw: &'a FfiPreviewFrame) -> Self {
        Self { raw }
    }

    pub fn width(&self) -> u16 {
        self.raw.width
    }

    pub fn height(&self) -> u16 {
        self.raw.height
    }

    pub fn pixels(&self) -> &[u16] {
        unsafe { self.raw.pixels.as_slice() }
    }

    pub fn events(&self) -> &[FfiCdEvent] {
        unsafe { self.raw.events.as_slice() }
    }

    pub fn window_start_us(&self) -> u64 {
        self.raw.window_start_us
    }

    pub fn window_end_us(&self) -> u64 {
        self.raw.window_end_us
    }
}

pub struct HostOutput<'a> {
    raw: &'a mut FfiOutputCallbacks,
}

impl<'a> HostOutput<'a> {
    pub fn new(raw: &'a mut FfiOutputCallbacks) -> Self {
        Self { raw }
    }

    pub fn add_highlight_pixels(&mut self, pixels: &[FfiPixel], color: [u8; 4]) {
        unsafe {
            (self.raw.add_highlight_pixels)(
                self.raw.ctx,
                FfiSlice::from_slice(pixels),
                FfiColorRgba::from_rgba(color),
            );
        }
    }

    pub fn add_crosshair_markers(
        &mut self,
        markers: &[FfiSubpixelMarker],
        color: [u8; 4],
        arm_len: u16,
    ) {
        unsafe {
            (self.raw.add_crosshair_markers)(
                self.raw.ctx,
                FfiSlice::from_slice(markers),
                FfiColorRgba::from_rgba(color),
                arm_len,
            );
        }
    }

    pub fn add_marker_overlay(
        &mut self,
        markers: &[FfiMarkerOverlayItem],
        dataset_id: Option<&str>,
        layer_id: Option<&str>,
        source_label: Option<&str>,
    ) {
        unsafe {
            (self.raw.add_marker_overlay)(
                self.raw.ctx,
                FfiSlice::from_slice(markers),
                dataset_id.map_or_else(FfiString::empty, FfiString::from),
                layer_id.map_or_else(FfiString::empty, FfiString::from),
                source_label.map_or_else(FfiString::empty, FfiString::from),
            );
        }
    }

    pub fn add_warning(&mut self, source: &str, severity: AnalysisSeverity, message: &str) {
        unsafe {
            (self.raw.add_warning)(
                self.raw.ctx,
                FfiString::from(source),
                severity,
                FfiString::from(message),
            );
        }
    }
}

pub struct EventStoreHandle<'a> {
    raw: &'a FfiEventStoreHandle,
}

impl<'a> EventStoreHandle<'a> {
    pub fn new(raw: &'a FfiEventStoreHandle) -> Self {
        Self { raw }
    }

    pub fn frame_count(&self) -> usize {
        unsafe { (self.raw.frame_count)(self.raw.ctx) }
    }

    pub fn frame(&self, index: usize) -> Option<FfiEventFrame> {
        let mut out = FfiEventFrame::empty();
        let found = unsafe { (self.raw.frame_at)(self.raw.ctx, index, &mut out) };
        found.then_some(out)
    }

    pub fn frames(&self) -> Vec<FfiEventFrame> {
        let count = self.frame_count();
        let mut frames = Vec::with_capacity(count);
        for index in 0..count {
            if let Some(frame) = self.frame(index) {
                frames.push(frame);
            }
        }
        frames
    }

    pub fn frame_range_for_timestamps(&self, start_us: u64, end_us: u64) -> Option<(usize, usize)> {
        let mut out_start = 0usize;
        let mut out_end = 0usize;
        let found = unsafe {
            (self.raw.frame_range_for_timestamps)(
                self.raw.ctx,
                start_us,
                end_us,
                &mut out_start,
                &mut out_end,
            )
        };
        found.then_some((out_start, out_end))
    }

    pub fn frames_in_range(&self, start_us: u64, end_us: u64) -> Vec<FfiEventFrame> {
        let Some((start, end)) = self.frame_range_for_timestamps(start_us, end_us) else {
            return Vec::new();
        };

        let mut frames = Vec::with_capacity(end.saturating_sub(start));
        for index in start..end {
            if let Some(frame) = self.frame(index) {
                frames.push(frame);
            }
        }
        frames
    }

    pub fn collect_events_in_range(&self, start_us: u64, end_us: u64, out: &mut Vec<FfiCdEvent>) {
        out.clear();
        let Some((start, end)) = self.frame_range_for_timestamps(start_us, end_us) else {
            return;
        };

        let mut total_events = 0usize;
        for index in start..end {
            if let Some(frame) = self.frame(index) {
                total_events += unsafe { frame.as_slice() }.len();
            }
        }
        out.reserve(total_events);

        for index in start..end {
            let Some(frame) = self.frame(index) else {
                continue;
            };
            let events = unsafe { frame.as_slice() };
            out.extend_from_slice(augur_event_types::inclusive_window(
                events, start_us, end_us,
            ));
        }
    }

    pub fn oldest_timestamp_us(&self) -> u64 {
        unsafe { (self.raw.oldest_timestamp_us)(self.raw.ctx) }
    }
}

pub struct HostContext<'a> {
    raw: &'a mut FfiPluginContext,
}

impl<'a> HostContext<'a> {
    pub fn new(raw: &'a mut FfiPluginContext) -> Self {
        Self { raw }
    }

    pub fn raw_events(&self) -> &[FfiCdEvent] {
        unsafe { self.raw.raw_events.as_slice() }
    }

    pub fn publish_raw(&mut self, key: &str, value: &[u8]) {
        unsafe {
            (self.raw.publish)(
                self.raw.ctx,
                FfiString::from(key),
                FfiSlice::from_slice(value),
            );
        }
    }

    pub fn publish<T: Serialize>(&mut self, key: &str, value: &T) -> Result<(), serde_json::Error> {
        let json = serde_json::to_vec(value)?;
        self.publish_raw(key, &json);
        Ok(())
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, serde_json::Error> {
        self.get_with(self.raw.get, key)
    }

    pub fn publish_persistent_raw(&mut self, key: &str, value: &[u8]) {
        unsafe {
            (self.raw.publish_persistent)(
                self.raw.ctx,
                FfiString::from(key),
                FfiSlice::from_slice(value),
            );
        }
    }

    pub fn publish_persistent<T: Serialize>(
        &mut self,
        key: &str,
        value: &T,
    ) -> Result<(), serde_json::Error> {
        let json = serde_json::to_vec(value)?;
        self.publish_persistent_raw(key, &json);
        Ok(())
    }

    pub fn get_persistent<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        self.get_with(self.raw.get_persistent, key)
    }

    fn get_with<T: DeserializeOwned>(
        &self,
        callback: unsafe extern "C" fn(
            *mut std::ffi::c_void,
            FfiString,
            *mut *const u8,
            *mut usize,
        ) -> bool,
        key: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        let found = unsafe {
            callback(
                self.raw.ctx,
                FfiString::from(key),
                &mut out_ptr,
                &mut out_len,
            )
        };
        if !found || out_ptr.is_null() || out_len == 0 {
            return Ok(None);
        }

        let bytes = unsafe { std::slice::from_raw_parts(out_ptr, out_len) };
        serde_json::from_slice(bytes).map(Some)
    }
}

pub trait Plugin: Default {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str {
        ""
    }

    fn enabled(&self) -> bool;

    fn set_enabled(&mut self, enabled: bool);

    fn reset(&mut self);

    fn input_kind(&self) -> PluginInput {
        PluginInput::FrameOnly
    }

    fn capabilities(&self) -> PluginCapabilities {
        PluginCapabilities::default()
    }

    fn dependencies(&self) -> &[&'static str] {
        &[]
    }

    fn process_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        context: &mut HostContext<'_>,
        event_store: &EventStoreHandle<'_>,
    );

    fn settings_schema(&self) -> SettingsSchema {
        SettingsSchema::default()
    }

    fn get_setting(&self, _key: &str) -> Option<serde_json::Value> {
        None
    }

    fn set_setting(&mut self, _key: &str, _value: serde_json::Value) -> Result<(), String> {
        Err("unknown setting".into())
    }

    fn status_entries(&self) -> Vec<StatusEntry> {
        Vec::new()
    }

    fn host_views(&self) -> HostViewRegistry {
        HostViewRegistry::default()
    }

    fn host_view_dataset(&self, _dataset_id: &str) -> Option<Vec<u8>> {
        None
    }

    fn host_view_dataset_generation(&self, _dataset_id: &str) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use super::{EventStoreHandle, HostContext};
    use crate::{
        EventStore, FfiCdEvent, FfiEventFrame, FfiEventStoreHandle, FfiPluginContext, FfiSlice,
        FfiString,
    };

    #[derive(Default)]
    struct PublishCapture {
        transient: Vec<(String, Vec<u8>)>,
        persistent: Vec<(String, Vec<u8>)>,
    }

    unsafe extern "C" fn publish_transient(ctx: *mut c_void, key: FfiString, value: FfiSlice<u8>) {
        let capture = unsafe { &mut *(ctx.cast::<PublishCapture>()) };
        let key = unsafe { key.as_str().expect("ffi key must be utf-8") }.to_owned();
        let value = unsafe { value.as_slice() }.to_vec();
        capture.transient.push((key, value));
    }

    unsafe extern "C" fn publish_persistent(ctx: *mut c_void, key: FfiString, value: FfiSlice<u8>) {
        let capture = unsafe { &mut *(ctx.cast::<PublishCapture>()) };
        let key = unsafe { key.as_str().expect("ffi key must be utf-8") }.to_owned();
        let value = unsafe { value.as_slice() }.to_vec();
        capture.persistent.push((key, value));
    }

    unsafe extern "C" fn get_missing(
        _ctx: *mut c_void,
        _key: FfiString,
        out_ptr: *mut *const u8,
        out_len: *mut usize,
    ) -> bool {
        if !out_ptr.is_null() {
            unsafe {
                *out_ptr = std::ptr::null();
            }
        }
        if !out_len.is_null() {
            unsafe {
                *out_len = 0;
            }
        }
        false
    }

    fn event(timestamp: u64, x: u16) -> FfiCdEvent {
        FfiCdEvent::new(x, 0, timestamp, 1)
    }

    unsafe extern "C" fn frame_count(ctx: *const c_void) -> usize {
        let store = unsafe { &*(ctx.cast::<EventStore>()) };
        store.frame_count()
    }

    unsafe extern "C" fn frame_at(
        ctx: *const c_void,
        index: usize,
        out_frame: *mut FfiEventFrame,
    ) -> bool {
        let store = unsafe { &*(ctx.cast::<EventStore>()) };
        let Some(frame) = store.frame(index) else {
            return false;
        };
        let Some(slot) = (unsafe { out_frame.as_mut() }) else {
            return false;
        };
        *slot = frame;
        true
    }

    unsafe extern "C" fn frame_range_for_timestamps(
        ctx: *const c_void,
        start_us: u64,
        end_us: u64,
        out_start: *mut usize,
        out_end: *mut usize,
    ) -> bool {
        let store = unsafe { &*(ctx.cast::<EventStore>()) };
        let Some((start, end)) = store.frame_range_for_timestamps(start_us, end_us) else {
            if !out_start.is_null() {
                unsafe {
                    *out_start = 0;
                }
            }
            if !out_end.is_null() {
                unsafe {
                    *out_end = 0;
                }
            }
            return false;
        };
        if !out_start.is_null() {
            unsafe {
                *out_start = start;
            }
        }
        if !out_end.is_null() {
            unsafe {
                *out_end = end;
            }
        }
        true
    }

    unsafe extern "C" fn oldest_timestamp_us(ctx: *const c_void) -> u64 {
        let store = unsafe { &*(ctx.cast::<EventStore>()) };
        store.oldest_timestamp_us().unwrap_or(0)
    }

    #[test]
    fn host_context_supports_raw_and_json_publishing() {
        let mut capture = PublishCapture::default();
        let mut ffi = FfiPluginContext {
            ctx: &mut capture as *mut _ as *mut c_void,
            raw_events: FfiSlice::empty(),
            publish: publish_transient,
            get: get_missing,
            publish_persistent,
            get_persistent: get_missing,
        };
        let mut host = HostContext::new(&mut ffi);

        host.publish_raw("raw.topic", b"abc");
        host.publish("json.topic", &7u32).expect("json publish");
        host.publish_persistent_raw("raw.persist", b"xyz");
        host.publish_persistent("json.persist", &vec![1u8, 2])
            .expect("json persistent publish");

        assert_eq!(
            capture.transient,
            vec![
                ("raw.topic".into(), b"abc".to_vec()),
                ("json.topic".into(), b"7".to_vec()),
            ]
        );
        assert_eq!(
            capture.persistent,
            vec![
                ("raw.persist".into(), b"xyz".to_vec()),
                ("json.persist".into(), b"[1,2]".to_vec()),
            ]
        );
    }

    #[test]
    fn event_store_handle_exposes_frame_based_queries() {
        let mut store = EventStore::default();
        store.push_frame(&[event(10, 1), event(20, 2)], 10, 20);
        store.push_frame(&[event(30, 3), event(40, 4)], 30, 40);

        let ffi = FfiEventStoreHandle {
            ctx: &store as *const _ as *const c_void,
            frame_count,
            frame_at,
            frame_range_for_timestamps,
            oldest_timestamp_us,
        };
        let handle = EventStoreHandle::new(&ffi);

        assert_eq!(handle.frame_count(), 2);
        assert_eq!(handle.oldest_timestamp_us(), 10);

        let first = handle.frame(0).expect("first frame");
        assert_eq!(unsafe { first.as_slice() }, &[event(10, 1), event(20, 2)]);

        let frames = handle.frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(
            unsafe { frames[1].as_slice() },
            &[event(30, 3), event(40, 4)]
        );

        let ranged = handle.frames_in_range(15, 35);
        assert_eq!(ranged.len(), 2);
        assert_eq!(
            unsafe { ranged[0].as_slice() },
            &[event(10, 1), event(20, 2)]
        );

        let mut flattened = Vec::new();
        handle.collect_events_in_range(15, 35, &mut flattened);
        assert_eq!(flattened, &[event(20, 2), event(30, 3)]);
    }
}
