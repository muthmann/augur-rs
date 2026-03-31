mod context;
mod event_store;
mod ffi;
mod helpers;
mod macros;
mod settings;

pub use context::{
    GlobalSettings, HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor, HostViewKind,
    HostViewPlacement, HostViewRegistry, Localization, LocalizationResults, LocalizationRow,
    LocalizationTable, TableColumn, TableColumnData, TableColumnValues, TableCoordinateSpace2d,
    TableDatasetV1, TableSchema, TableValueType, CTX_GLOBAL_SETTINGS, CTX_LOCALIZATION_RESULTS,
};
pub use event_store::EventStore;
pub use ffi::{
    AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, FfiCdEvent,
    FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiOutputCallbacks, FfiPixel,
    FfiPluginContext, FfiPreviewFrame, FfiSlice, FfiString, FfiSubpixelMarker,
    HostViewDatasetGenerationFn, PluginEntry, PluginInput, PluginVTable, PLUGIN_ENTRY_SYMBOL,
};
pub use helpers::{EventStoreHandle, HostContext, HostOutput, Plugin, PluginFrame};
pub use settings::{SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry};

#[doc(hidden)]
pub mod __private {
    use std::cell::RefCell;

    /// Stores `bytes` in `scratch` and exposes the buffer through raw out-pointers.
    ///
    /// # Safety
    /// When non-null, `out_ptr` and `out_len` must be valid writable pointers for one value each.
    pub unsafe fn write_bytes(
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

    /// Clears raw out-pointers used for returning borrowed byte buffers.
    ///
    /// # Safety
    /// When non-null, `out_ptr` and `out_len` must be valid writable pointers for one value each.
    pub unsafe fn clear_out_bytes(out_ptr: *mut *const u8, out_len: *mut usize) {
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
        context::{
            GlobalSettings, HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor,
            HostViewKind, HostViewPlacement, HostViewRegistry, Localization, LocalizationResults,
            LocalizationRow, LocalizationTable, TableColumn, TableColumnData, TableColumnValues,
            TableCoordinateSpace2d, TableDatasetV1, TableSchema, TableValueType,
            CTX_GLOBAL_SETTINGS, CTX_LOCALIZATION_RESULTS,
        },
        settings::{SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry},
        AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, FfiCdEvent,
        FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiPixel, FfiSlice, FfiString,
        FfiSubpixelMarker, HostViewDatasetGenerationFn, PluginInput, PluginVTable,
    };

    #[test]
    fn ffi_layouts_are_stable() {
        assert_eq!(std::mem::size_of::<FfiSlice<u8>>(), 16);
        assert_eq!(std::mem::size_of::<FfiString>(), 16);
        assert_eq!(std::mem::size_of::<FfiCdEvent>(), 16);
        assert_eq!(std::mem::size_of::<FfiEventFrame>(), 32);
        assert_eq!(std::mem::size_of::<FfiColorRgba>(), 4);
        assert_eq!(std::mem::size_of::<FfiPixel>(), 4);
        assert_eq!(std::mem::size_of::<FfiSubpixelMarker>(), 8);
        assert_eq!(std::mem::size_of::<FfiEventStoreHandle>(), 40);
        assert_eq!(std::mem::size_of::<PluginVTable>(), 160);
        assert_eq!(std::mem::size_of::<EventStoreFrameAtFn>(), 8);
        assert_eq!(
            std::mem::size_of::<EventStoreFrameRangeForTimestampsFn>(),
            8
        );
        assert_eq!(std::mem::size_of::<HostViewDatasetGenerationFn>(), 8);
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
    fn global_settings_round_trip_through_json() {
        let settings = GlobalSettings {
            nm_per_pixel: 65.0,
            sensor_width: 1280,
            sensor_height: 720,
            acq_time_ms: 50,
            event_store_budget_bytes: 100 * 1024 * 1024,
        };

        let json = serde_json::to_vec(&settings).expect("settings must serialize");
        let decoded: GlobalSettings =
            serde_json::from_slice(&json).expect("settings must deserialize");
        assert_eq!(decoded, settings);
        assert_eq!(CTX_GLOBAL_SETTINGS, "augur.global_settings");
    }

    #[test]
    fn localization_table_round_trips_through_json() {
        let table = LocalizationTable {
            rows: vec![LocalizationRow {
                id: 7,
                frame: 3,
                x_nm: 812.5,
                y_nm: 455.0,
                sigma_nm: 132.0,
                intensity: 4_200.0,
                offset: 14.0,
                uncertainty_xy_nm: 19.5,
                timestamp_us: 12_345,
            }],
            nm_per_pixel: 65.0,
            sensor_width: 1280,
            sensor_height: 720,
        };

        let json = serde_json::to_vec(&table).expect("table must serialize");
        let decoded: LocalizationTable =
            serde_json::from_slice(&json).expect("table must deserialize");
        assert_eq!(decoded, table);
    }

    #[test]
    fn ffi_string_and_slice_helpers_preserve_contents() {
        let text = "hello";
        let ffi = FfiString::from(text);
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

    #[test]
    fn host_view_registry_round_trips_through_json() {
        let registry = HostViewRegistry {
            datasets: vec![HostDatasetDescriptor {
                id: "table.localization".into(),
                title: "Localizations".into(),
                kind: HostDatasetKind::TableV1(TableSchema {
                    columns: vec![
                        TableColumn {
                            id: "frame".into(),
                            title: "Frame".into(),
                            value_type: TableValueType::U64,
                        },
                        TableColumn {
                            id: "x_nm".into(),
                            title: "X [nm]".into(),
                            value_type: TableValueType::F64,
                        },
                        TableColumn {
                            id: "y_nm".into(),
                            title: "Y [nm]".into(),
                            value_type: TableValueType::F64,
                        },
                    ],
                    coordinate_space_2d: Some(TableCoordinateSpace2d {
                        x_column: "x_nm".into(),
                        y_column: "y_nm".into(),
                        x_min: 0.0,
                        x_max: 100.0,
                        y_min: 0.0,
                        y_max: 100.0,
                    }),
                }),
                empty_message: "No rows yet".into(),
            }],
            views: vec![
                HostViewDescriptor {
                    id: "panel.localization".into(),
                    title: "Localization Preview".into(),
                    dataset_id: "table.localization".into(),
                    placement: HostViewPlacement::AnalysisPanel,
                    kind: HostViewKind::CompactTable,
                },
                HostViewDescriptor {
                    id: "window.localization".into(),
                    title: "Localization Window".into(),
                    dataset_id: "table.localization".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::Density2dFromTable {
                        x_column: "x_nm".into(),
                        y_column: "y_nm".into(),
                    },
                },
            ],
        };

        let json = serde_json::to_vec(&registry).expect("registry must serialize");
        let decoded: HostViewRegistry =
            serde_json::from_slice(&json).expect("registry must deserialize");
        assert_eq!(decoded, registry);
    }

    #[test]
    fn table_dataset_v1_round_trips_through_json() {
        let dataset = TableDatasetV1::new(vec![
            TableColumnData {
                column_id: "frame".into(),
                values: TableColumnValues::U64(vec![1, 2]),
            },
            TableColumnData {
                column_id: "x_nm".into(),
                values: TableColumnValues::F64(vec![12.5, 13.5]),
            },
        ])
        .expect("dataset must validate");

        let json = serde_json::to_vec(&dataset).expect("dataset must serialize");
        let decoded: TableDatasetV1 =
            serde_json::from_slice(&json).expect("dataset must deserialize");
        assert_eq!(decoded, dataset);
        assert_eq!(decoded.row_count(), 2);
    }

    #[test]
    fn table_dataset_v1_rejects_mismatched_column_lengths() {
        let json = serde_json::json!({
            "columns": [
                {
                    "column_id": "frame",
                    "values": {
                        "value_type": "u64",
                        "values": [1, 2]
                    }
                },
                {
                    "column_id": "x_nm",
                    "values": {
                        "value_type": "f64",
                        "values": [12.5]
                    }
                }
            ]
        });

        let err = serde_json::from_value::<TableDatasetV1>(json)
            .expect_err("dataset must reject mismatched lengths");
        assert!(err.to_string().contains("identical lengths"));
    }
}
