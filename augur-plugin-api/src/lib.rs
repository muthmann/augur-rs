mod context;
mod ffi;
mod helpers;
mod macros;
mod settings;

pub use context::{Localization, LocalizationResults, CTX_LOCALIZATION_RESULTS};
pub use ffi::{
    AnalysisSeverity, FfiCdEvent, FfiColorRgba, FfiOutputCallbacks, FfiPixel, FfiPluginContext,
    FfiPreviewFrame, FfiSlice, FfiString, FfiSubpixelMarker, PluginEntry, PluginInput,
    PluginVTable, PLUGIN_ENTRY_SYMBOL,
};
pub use helpers::{HostContext, HostOutput, Plugin, PluginFrame};
pub use settings::{SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry};

#[doc(hidden)]
pub mod __private {
    use std::cell::RefCell;

    pub fn write_bytes(
        scratch: &RefCell<Vec<u8>>,
        bytes: Vec<u8>,
        out_ptr: *mut *const u8,
        out_len: *mut usize,
    ) {
        if out_ptr.is_null() || out_len.is_null() {
            return;
        }

        let mut slot = scratch.borrow_mut();
        *slot = bytes;
        unsafe {
            *out_ptr = slot.as_ptr();
            *out_len = slot.len();
        }
    }

    pub fn clear_out_bytes(out_ptr: *mut *const u8, out_len: *mut usize) {
        if out_ptr.is_null() || out_len.is_null() {
            return;
        }
        unsafe {
            *out_ptr = std::ptr::null();
            *out_len = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        context::{Localization, LocalizationResults, CTX_LOCALIZATION_RESULTS},
        settings::{SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry},
        AnalysisSeverity, FfiCdEvent, FfiColorRgba, FfiPixel, FfiSlice, FfiString,
        FfiSubpixelMarker, PluginInput,
    };

    #[test]
    fn ffi_layouts_are_stable() {
        assert_eq!(std::mem::size_of::<FfiSlice<u8>>(), 16);
        assert_eq!(std::mem::size_of::<FfiString>(), 16);
        assert_eq!(std::mem::size_of::<FfiCdEvent>(), 16);
        assert_eq!(std::mem::size_of::<FfiColorRgba>(), 4);
        assert_eq!(std::mem::size_of::<FfiPixel>(), 4);
        assert_eq!(std::mem::size_of::<FfiSubpixelMarker>(), 8);
    }

    #[test]
    fn settings_schema_round_trips_through_json() {
        let schema = SettingsSchema {
            sections: vec![SettingsSection {
                label: "Main".into(),
                description: Some("Example section".into()),
                default_open: true,
                items: vec![
                    SettingItem {
                        key: "enabled".into(),
                        label: "Enabled".into(),
                        tooltip: Some("Turns the plugin on.".into()),
                        kind: SettingKind::Bool { default: true },
                    },
                    SettingItem {
                        key: "mode".into(),
                        label: "Mode".into(),
                        tooltip: None,
                        kind: SettingKind::Enum {
                            variants: vec!["One".into(), "Two".into()],
                            default: 1,
                        },
                    },
                ],
            }],
        };

        let json = serde_json::to_vec(&schema).expect("schema must serialize");
        let decoded: SettingsSchema =
            serde_json::from_slice(&json).expect("schema must deserialize");
        assert_eq!(decoded, schema);
    }

    #[test]
    fn shared_context_types_round_trip_through_json() {
        let results = LocalizationResults {
            localizations: vec![Localization {
                x: 12.5,
                y: 7.25,
                sigma_x: 1.4,
                sigma_y: 1.6,
                amplitude: 42.0,
                background: 2.5,
                timestamp_us: 1_234,
                fit_error: 0.12,
            }],
            frame_window_start_us: 10,
            frame_window_end_us: 110,
        };

        let json = serde_json::to_vec(&results).expect("results must serialize");
        let decoded: LocalizationResults =
            serde_json::from_slice(&json).expect("results must deserialize");
        assert_eq!(decoded, results);
        assert_eq!(CTX_LOCALIZATION_RESULTS, "augur.localization.results");
    }

    #[test]
    fn ffi_string_and_slice_helpers_preserve_contents() {
        let text = "hello";
        let ffi = FfiString::from_str(text);
        let numbers = [1u8, 2, 3];
        let slice = FfiSlice::from_slice(&numbers);

        let decoded_text = unsafe { ffi.as_str().expect("ffi string must be utf-8") };
        let decoded_numbers = unsafe { slice.as_slice() };

        assert_eq!(decoded_text, text);
        assert_eq!(decoded_numbers, numbers);
    }

    #[test]
    fn enums_serialize_stably() {
        let phase = serde_json::to_string(&PluginInput::DerivedData).expect("phase json");
        let severity = serde_json::to_string(&AnalysisSeverity::Warning).expect("severity json");
        let status = serde_json::to_string(&StatusEntry::Text("ok".into())).expect("status json");

        assert_eq!(phase, "\"derived_data\"");
        assert_eq!(severity, "\"warning\"");
        assert!(status.contains("\"text\""));
    }
}
