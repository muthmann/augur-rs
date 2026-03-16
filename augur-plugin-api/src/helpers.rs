use serde::{de::DeserializeOwned, Serialize};

use crate::{
    ffi::{
        AnalysisSeverity, FfiCdEvent, FfiColorRgba, FfiOutputCallbacks, FfiPixel, FfiPluginContext,
        FfiPreviewFrame, FfiSlice, FfiString, FfiSubpixelMarker, PluginInput,
    },
    settings::{SettingsSchema, StatusEntry},
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

    pub fn publish<T: Serialize>(&mut self, key: &str, value: &T) -> Result<(), serde_json::Error> {
        let json = serde_json::to_vec(value)?;
        unsafe {
            (self.raw.publish)(
                self.raw.ctx,
                FfiString::from(key),
                FfiSlice::from_slice(&json),
            );
        }
        Ok(())
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, serde_json::Error> {
        let mut out_ptr = std::ptr::null();
        let mut out_len = 0usize;
        let found = unsafe {
            (self.raw.get)(
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

    fn dependencies(&self) -> &[&'static str] {
        &[]
    }

    fn process_frame(
        &mut self,
        frame: &PluginFrame<'_>,
        output: &mut HostOutput<'_>,
        context: &mut HostContext<'_>,
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

    fn accumulated_localizations(&self) -> Option<Vec<u8>> {
        None
    }
}
