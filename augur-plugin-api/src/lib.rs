mod context;
mod event_store;
mod ffi;
mod helpers;
mod macros;
mod settings;

pub use context::{
    GlobalSettings, HostActionDescriptor, HostActionRequest, HostActionRequestQueue,
    HostActionScope, HostActionScopePayload, HostDatasetDescriptor, HostDatasetDisplayMetadata,
    HostDatasetKind, HostDatasetRelation, HostMarkerShape, HostViewDescriptor, HostViewKind,
    HostViewPlacement, HostViewRegistry, Image2dV1, Series1dLine, Series1dPoint, Series1dV1,
    TableColumn, TableColumnData, TableColumnDisplayEntry, TableColumnDisplayFormat,
    TableColumnDisplayMetadata, TableColumnValues, TableColumnWidthPriority,
    TableCoordinateSpace2d, TableCoordinateSpace3d, TableDatasetV1, TableRowProvenance,
    TableSchema, TableValueType, CTX_GLOBAL_SETTINGS, CTX_INVESTIGATION_ACTION_REQUESTS,
    HOST_ACTION_CLUSTER_ROWS_PARAM,
};
pub use event_store::EventStore;
pub use ffi::{
    AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, FfiCdEvent,
    FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiMarkerOverlayItem, FfiMarkerShape,
    FfiOutputCallbacks, FfiPixel, FfiPluginContext, FfiPreviewFrame, FfiSlice, FfiString,
    FfiSubpixelMarker, HostViewDatasetGenerationFn, PluginCapabilities, PluginCapabilitiesFn,
    PluginEntry, PluginInput, PluginVTable, PLUGIN_ABI_VERSION, PLUGIN_ENTRY_SYMBOL,
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
            GlobalSettings, HostActionDescriptor, HostActionRequest, HostActionRequestQueue,
            HostActionScope, HostActionScopePayload, HostDatasetDescriptor,
            HostDatasetDisplayMetadata, HostDatasetKind, HostMarkerShape, HostViewDescriptor,
            HostViewKind, HostViewPlacement, HostViewRegistry, Image2dV1, Series1dLine,
            Series1dPoint, Series1dV1, TableColumn, TableColumnData, TableColumnValues,
            TableCoordinateSpace2d, TableCoordinateSpace3d, TableDatasetV1, TableSchema,
            TableValueType, CTX_GLOBAL_SETTINGS, CTX_INVESTIGATION_ACTION_REQUESTS,
            HOST_ACTION_CLUSTER_ROWS_PARAM,
        },
        settings::{SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry},
        AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, FfiCdEvent,
        FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiMarkerOverlayItem, FfiMarkerShape,
        FfiPixel, FfiSlice, FfiString, FfiSubpixelMarker, HostViewDatasetGenerationFn,
        PluginCapabilities, PluginCapabilitiesFn, PluginInput, PluginVTable, PLUGIN_ABI_VERSION,
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
        assert_eq!(std::mem::size_of::<FfiMarkerShape>(), 4);
        assert_eq!(std::mem::size_of::<FfiMarkerOverlayItem>(), 88);
        assert_eq!(std::mem::size_of::<FfiEventStoreHandle>(), 40);
        assert_eq!(std::mem::size_of::<PluginVTable>(), 168);
        assert_eq!(std::mem::size_of::<EventStoreFrameAtFn>(), 8);
        assert_eq!(
            std::mem::size_of::<EventStoreFrameRangeForTimestampsFn>(),
            8
        );
        assert_eq!(std::mem::size_of::<HostViewDatasetGenerationFn>(), 8);
        assert_eq!(std::mem::size_of::<PluginCapabilitiesFn>(), 8);
        assert_eq!(PLUGIN_ABI_VERSION, 4);
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
    fn plugin_capabilities_default_to_no_optional_features() {
        let capabilities = PluginCapabilities::default();
        assert!(!capabilities.retained_event_history);
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
            datasets: vec![
                HostDatasetDescriptor {
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
                        coordinate_space_3d: Some(TableCoordinateSpace3d {
                            x_column: "x_nm".into(),
                            y_column: "y_nm".into(),
                            z_column: "frame".into(),
                            x_min: 0.0,
                            x_max: 100.0,
                            y_min: 0.0,
                            y_max: 100.0,
                            z_min: 0.0,
                            z_max: 20.0,
                        }),
                        row_id_column: Some("frame".into()),
                        time_column: Some("frame".into()),
                        layer_id: Some("reconstruction".into()),
                        semantic_label: Some("localization".into()),
                        provenance: None,
                        column_display: Vec::new(),
                    }),
                    empty_message: "No rows yet".into(),
                    display: Some(HostDatasetDisplayMetadata {
                        layer_title: Some("Reconstruction".into()),
                        default_visibility: Some(true),
                        default_color: Some([255, 180, 80, 255]),
                        default_marker_shape: Some(HostMarkerShape::Circle),
                        default_size: Some(3.5),
                    }),
                    relations: Vec::new(),
                },
                HostDatasetDescriptor {
                    id: "image.preview".into(),
                    title: "Rendered Image".into(),
                    kind: HostDatasetKind::Image2dV1,
                    empty_message: "No image yet".into(),
                    display: None,
                    relations: Vec::new(),
                },
                HostDatasetDescriptor {
                    id: "series.focus".into(),
                    title: "Focus Series".into(),
                    kind: HostDatasetKind::Series1dV1,
                    empty_message: "No samples yet".into(),
                    display: None,
                    relations: Vec::new(),
                },
            ],
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
                HostViewDescriptor {
                    id: "window.scatter3d".into(),
                    title: "Localization 3D".into(),
                    dataset_id: "table.localization".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::Scatter3dFromTable {
                        x_column: "x_nm".into(),
                        y_column: "y_nm".into(),
                        z_column: "frame".into(),
                    },
                },
                HostViewDescriptor {
                    id: "window.scatter".into(),
                    title: "Localization Scatter".into(),
                    dataset_id: "table.localization".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::Scatter2dFromTable {
                        x_column: "x_nm".into(),
                        y_column: "y_nm".into(),
                    },
                },
                HostViewDescriptor {
                    id: "window.image".into(),
                    title: "Rendered Image".into(),
                    dataset_id: "image.preview".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::ImageWindow,
                },
                HostViewDescriptor {
                    id: "window.series".into(),
                    title: "Focus Series".into(),
                    dataset_id: "series.focus".into(),
                    placement: HostViewPlacement::Window,
                    kind: HostViewKind::LineSeriesWindow,
                },
            ],
            actions: vec![HostActionDescriptor {
                id: "refit_cluster".into(),
                title: "Tune fit on this cluster".into(),
                scope: HostActionScope::Cluster {
                    dataset_id: "table.candidates".into(),
                    group_column: "cluster_id".into(),
                },
                param_schema: Some(serde_json::json!({
                    "sections": [{
                        "label": "Fit",
                        "items": [{
                            "key": "sigma_max",
                            "label": "σ max",
                            "kind": { "type": "f64", "default": 3.0 }
                        }]
                    }]
                })),
            }],
        };

        let json = serde_json::to_vec(&registry).expect("registry must serialize");
        let decoded: HostViewRegistry =
            serde_json::from_slice(&json).expect("registry must deserialize");
        assert_eq!(decoded, registry);
    }

    #[test]
    fn host_action_request_queue_round_trips_through_json() {
        let queue = HostActionRequestQueue {
            requests: vec![
                HostActionRequest {
                    request_id: 1,
                    action_id: "refit_cluster".into(),
                    scope_payload: HostActionScopePayload::Cluster {
                        dataset_id: "table.candidates".into(),
                        group_column: "cluster_id".into(),
                        group_value: "7".into(),
                    },
                    params: serde_json::json!({ "sigma_max": 3.5 }),
                },
                HostActionRequest {
                    request_id: 2,
                    action_id: "commit_refit".into(),
                    scope_payload: HostActionScopePayload::Row {
                        dataset_id: "table.refit_preview".into(),
                        row_id: "refit-42".into(),
                    },
                    params: serde_json::Value::Null,
                },
            ],
        };

        let json = serde_json::to_vec(&queue).expect("queue must serialize");
        let decoded: HostActionRequestQueue =
            serde_json::from_slice(&json).expect("queue must deserialize");
        assert_eq!(decoded, queue);
        assert_eq!(
            CTX_INVESTIGATION_ACTION_REQUESTS,
            "augur.investigation.action_requests"
        );
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

    #[test]
    fn image_dataset_v1_round_trips_through_json() {
        let image = Image2dV1::new(2, 2, vec![0.0, 1.0, 2.0, 3.0]).expect("image must validate");

        let json = serde_json::to_vec(&image).expect("image must serialize");
        let decoded: Image2dV1 = serde_json::from_slice(&json).expect("image must deserialize");
        assert_eq!(decoded, image);
        assert_eq!(decoded.size(), [2, 2]);
    }

    #[test]
    fn image_dataset_v1_rejects_dimension_mismatches() {
        let json = serde_json::json!({
            "width": 2,
            "height": 2,
            "pixels": [0.0, 1.0, 2.0]
        });

        let err = serde_json::from_value::<Image2dV1>(json)
            .expect_err("image must reject mismatched dimensions");
        assert!(err.to_string().contains("pixel count"));
    }

    #[test]
    fn series_dataset_v1_round_trips_through_json() {
        let dataset = Series1dV1 {
            x_label: "Frame".into(),
            y_label: "Score".into(),
            lines: vec![Series1dLine {
                name: "Focus".into(),
                points: vec![
                    Series1dPoint { x: 0.0, y: 1.0 },
                    Series1dPoint { x: 1.0, y: 1.5 },
                ],
            }],
        };

        let json = serde_json::to_vec(&dataset).expect("series must serialize");
        let decoded: Series1dV1 = serde_json::from_slice(&json).expect("series must deserialize");
        assert_eq!(decoded, dataset);
        assert_eq!(decoded.total_points(), 2);
    }
}
