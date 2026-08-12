mod context;
mod event_store;
mod ffi;
mod helpers;
mod macros;
mod settings;

pub use context::{
    CameraBiasOffsetsV1, CameraConfigurationProvenanceV1, CameraConfigurationSnapshotV1,
    CameraDigitalFilterV1, CameraExternalTriggerV1, CameraGlobalSettingsV1, EventFiltersV1,
    GlobalSettings, HostActionDescriptor, HostActionRequest, HostActionRequestQueue,
    HostActionScope, HostActionScopePayload, HostCommand, HostCommandOutcome, HostCommandReply,
    HostCommandRequest, HostDatasetDescriptor, HostDatasetDisplayMetadata, HostDatasetKind,
    HostDatasetRelation, HostMarkerShape, HostViewDescriptor, HostViewKind, HostViewPlacement,
    HostViewRegistry, Image2dV1, PluginControlInbox, PluginControlSnapshot, PluginServiceOutcome,
    PluginServiceReply, PluginServiceRequest, RoiV1, SensorBiasCodesV1, SensorBiasOffsetsV1,
    SensorBiasReadbackV1, SensorMonitoringV1, Series1dLine, Series1dPoint, Series1dV1, TableColumn,
    TableColumnData, TableColumnDisplayEntry, TableColumnDisplayFormat, TableColumnDisplayMetadata,
    TableColumnValues, TableColumnWidthPriority, TableCoordinateSpace2d, TableCoordinateSpace3d,
    TableDatasetV1, TableRowProvenance, TableSchema, TableValueType, CTX_GLOBAL_SETTINGS,
    CTX_INVESTIGATION_ACTION_REQUESTS, CTX_SENSOR_MONITORING, HOST_ACTION_CLUSTER_ROWS_PARAM,
};
pub use event_store::EventStore;
pub use ffi::{
    AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, ExecutionMode,
    FfiCdEvent, FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiExecutionContext,
    FfiExternalTriggerEvent, FfiMarkerOverlayItem, FfiMarkerShape, FfiOutputCallbacks, FfiPixel,
    FfiPluginContext, FfiPluginControlContext, FfiPreviewFrame, FfiSlice, FfiString,
    FfiSubpixelMarker, HostViewDatasetGenerationFn, PluginCapabilities, PluginCapabilitiesFn,
    PluginDiscontinuity, PluginDiscontinuityFn, PluginEntry, PluginInput, PluginRuntimeRole,
    PluginRuntimeRoleFn, PluginStateKind, PluginStateKindFn, PluginVTable, PLUGIN_ABI_VERSION,
    PLUGIN_ENTRY_SYMBOL,
};
pub use helpers::{
    EventStoreHandle, ExecutionContext, HostContext, HostOutput, Plugin, PluginControlContext,
    PluginFrame,
};
pub use settings::{
    PathDialogKind, SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry,
};

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
            EventFiltersV1, GlobalSettings, HostActionDescriptor, HostActionRequest,
            HostActionRequestQueue, HostActionScope, HostActionScopePayload, HostDatasetDescriptor,
            HostDatasetDisplayMetadata, HostDatasetKind, HostMarkerShape, HostViewDescriptor,
            HostViewKind, HostViewPlacement, HostViewRegistry, Image2dV1, Series1dLine,
            Series1dPoint, Series1dV1, TableColumn, TableColumnData, TableColumnValues,
            TableCoordinateSpace2d, TableCoordinateSpace3d, TableDatasetV1, TableSchema,
            TableValueType, CTX_GLOBAL_SETTINGS, CTX_INVESTIGATION_ACTION_REQUESTS,
            CTX_SENSOR_MONITORING,
        },
        settings::{
            PathDialogKind, SettingItem, SettingKind, SettingsSchema, SettingsSection, StatusEntry,
        },
        AnalysisSeverity, EventStoreFrameAtFn, EventStoreFrameRangeForTimestampsFn, FfiCdEvent,
        FfiColorRgba, FfiEventFrame, FfiEventStoreHandle, FfiMarkerOverlayItem, FfiMarkerShape,
        FfiPixel, FfiPluginControlContext, FfiSlice, FfiString, FfiSubpixelMarker, HostCommand,
        HostCommandOutcome, HostCommandReply, HostCommandRequest, HostViewDatasetGenerationFn,
        PluginCapabilities, PluginCapabilitiesFn, PluginControlInbox, PluginControlSnapshot,
        PluginDiscontinuity, PluginDiscontinuityFn, PluginInput, PluginRuntimeRole,
        PluginServiceOutcome, PluginServiceReply, PluginServiceRequest, PluginStateKind,
        PluginStateKindFn, PluginVTable, PLUGIN_ABI_VERSION,
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
        assert_eq!(std::mem::size_of::<PluginVTable>(), 216);
        assert_eq!(std::mem::size_of::<FfiPluginControlContext>(), 72);
        assert_eq!(std::mem::size_of::<EventStoreFrameAtFn>(), 8);
        assert_eq!(
            std::mem::size_of::<EventStoreFrameRangeForTimestampsFn>(),
            8
        );
        assert_eq!(std::mem::size_of::<HostViewDatasetGenerationFn>(), 8);
        assert_eq!(std::mem::size_of::<PluginCapabilitiesFn>(), 8);
        assert_eq!(std::mem::size_of::<PluginStateKindFn>(), 8);
        assert_eq!(std::mem::size_of::<PluginDiscontinuityFn>(), 8);
        assert_eq!(std::mem::size_of::<PluginStateKind>(), 4);
        assert_eq!(std::mem::size_of::<PluginDiscontinuity>(), 4);
        assert_eq!(std::mem::size_of::<crate::FfiExternalTriggerEvent>(), 16);
        assert_eq!(std::mem::align_of::<crate::FfiExternalTriggerEvent>(), 8);
        assert_eq!(std::mem::size_of::<crate::ExecutionMode>(), 4);
        assert_eq!(std::mem::size_of::<PluginRuntimeRole>(), 4);
        assert_eq!(std::mem::size_of::<crate::FfiExecutionContext>(), 32);
        assert_eq!(PLUGIN_ABI_VERSION, 6);
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
                    SettingItem {
                        key: "run_name".into(),
                        label: "Run name".into(),
                        tooltip: None,
                        kind: SettingKind::Text {
                            default: "run".into(),
                        },
                    },
                    SettingItem {
                        key: "data_dir".into(),
                        label: "Data directory".into(),
                        tooltip: None,
                        kind: SettingKind::Path {
                            dialog: PathDialogKind::Directory,
                            default: String::new(),
                        },
                    },
                    SettingItem {
                        key: "save_snapshot".into(),
                        label: "Save snapshot".into(),
                        tooltip: None,
                        kind: SettingKind::Button { enabled: false },
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
    fn button_without_enabled_field_defaults_to_enabled() {
        // Schemas emitted by plugins built against the pre-`enabled` API.
        let decoded: SettingKind =
            serde_json::from_str(r#"{"kind":"button"}"#).expect("legacy button deserializes");
        assert_eq!(decoded, SettingKind::Button { enabled: true });
    }

    #[test]
    fn global_settings_round_trip_through_json() {
        let settings = GlobalSettings {
            nm_per_pixel: 65.0,
            sensor_width: 1280,
            sensor_height: 720,
            acq_time_ms: 50,
            event_store_budget_bytes: 100 * 1024 * 1024,
            record_sensor_telemetry: true,
            roi: crate::RoiV1 {
                x: 4,
                y: 8,
                width: 640,
                height: 480,
            },
            masked_pixels: vec![(1, 2), (3, 4)],
            event_filters: EventFiltersV1 {
                stc_enabled: false,
                trail_enabled: true,
                erc_enabled: false,
            },
        };

        let json = serde_json::to_vec(&settings).expect("settings must serialize");
        let decoded: GlobalSettings =
            serde_json::from_slice(&json).expect("settings must deserialize");
        assert_eq!(decoded, settings);
        assert_eq!(CTX_GLOBAL_SETTINGS, "augur.global_settings");
    }

    #[test]
    fn settings_from_a_host_with_no_filter_block_read_as_all_off() {
        // The field is additive: a plugin built against this API must still be
        // able to read what an older host published, and "no block" is the
        // same situation as "no filters", not a parse failure.
        let json = br#"{"nm_per_pixel":65.0,"sensor_width":1280,"sensor_height":720,
            "acq_time_ms":50,"event_store_budget_bytes":1024}"#;
        let decoded: GlobalSettings = serde_json::from_slice(json).expect("must deserialize");
        assert_eq!(decoded.event_filters, EventFiltersV1::default());
        assert!(!decoded.event_filters.stc_enabled);
    }

    #[test]
    fn apply_biases_names_only_the_two_threshold_biases() {
        // The narrowness is the point: no wire field exists for fo, hpf, refr,
        // the ROI or the pixel mask, so no plugin can reach them.
        let command = HostCommand::ApplyBiases {
            diff_on: Some(-20),
            diff_off: None,
        };
        let json = serde_json::to_string(&command).expect("must serialize");
        assert_eq!(
            json,
            r#"{"command":"apply_biases","diff_on":-20,"diff_off":null}"#
        );
        for forbidden in ["fo", "hpf", "refr", "roi", "mask"] {
            assert!(
                !json.contains(forbidden),
                "{forbidden} is reachable: {json}"
            );
        }
        let decoded: HostCommand = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(decoded, command);
    }

    #[test]
    fn a_bias_confirmation_carries_the_codes_the_sensor_reported() {
        let outcome = HostCommandOutcome::BiasesApplied {
            applied: crate::SensorBiasOffsetsV1 {
                diff_on: 12,
                diff_off: -8,
            },
            readback: crate::SensorBiasReadbackV1 {
                current: crate::SensorBiasCodesV1 {
                    diff_on: 114,
                    diff_off: 32,
                    fo: 55,
                    hpf: 0,
                    refr: 138,
                },
                factory_default: crate::SensorBiasCodesV1 {
                    diff_on: 102,
                    diff_off: 40,
                    fo: 55,
                    hpf: 0,
                    refr: 138,
                },
            },
            readback_age_s: 0.4,
        };
        let json = serde_json::to_vec(&outcome).expect("must serialize");
        let decoded: HostCommandOutcome = serde_json::from_slice(&json).expect("must deserialize");
        assert_eq!(decoded, outcome);
    }

    #[test]
    fn sensor_monitoring_round_trips_through_json() {
        let monitoring = crate::SensorMonitoringV1 {
            pixel_dead_time_us: Some(6.35),
            illumination_lux: Some(412.0),
            temperature_c: Some(41.3),
            bias_codes: Some(crate::SensorBiasReadbackV1 {
                current: crate::SensorBiasCodesV1 {
                    diff_on: 114,
                    diff_off: 40,
                    fo: 55,
                    hpf: 0,
                    refr: 118,
                },
                factory_default: crate::SensorBiasCodesV1 {
                    diff_on: 102,
                    diff_off: 40,
                    fo: 55,
                    hpf: 0,
                    refr: 138,
                },
            }),
            age_s: 0.25,
        };

        let json = serde_json::to_vec(&monitoring).expect("monitoring must serialize");
        let decoded: crate::SensorMonitoringV1 =
            serde_json::from_slice(&json).expect("monitoring must deserialize");
        assert_eq!(decoded, monitoring);
        assert_eq!(CTX_SENSOR_MONITORING, "augur.sensor_monitoring");
    }

    #[test]
    fn sensor_monitoring_tolerates_a_host_that_reports_nothing() {
        // A sensor without a monitoring block, and older hosts that only wrote
        // some of the fields, must both decode rather than fail the frame.
        let decoded: crate::SensorMonitoringV1 =
            serde_json::from_slice(br#"{"illumination_lux":12.5}"#)
                .expect("partial monitoring must deserialize");
        assert_eq!(decoded.illumination_lux, Some(12.5));
        assert_eq!(decoded.pixel_dead_time_us, None);
        assert_eq!(decoded.bias_codes, None);
        assert_eq!(decoded.age_s, 0.0);
    }

    #[test]
    fn plugin_capabilities_default_to_no_optional_features() {
        let capabilities = PluginCapabilities::default();
        assert!(!capabilities.retained_event_history);
    }

    #[test]
    fn plugin_state_kind_defaults_to_accumulating() {
        assert_eq!(PluginStateKind::default(), PluginStateKind::Accumulating);
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
    fn plugin_control_messages_round_trip_through_json() {
        let inbox = PluginControlInbox {
            service_replies: vec![PluginServiceReply {
                request_id: 7,
                source_plugin_id: "workflow.a1".into(),
                target_plugin_id: "device.modulation".into(),
                service: "apply.v1".into(),
                outcome: PluginServiceOutcome::Accepted {
                    payload: serde_json::json!({"revision": 3}),
                },
            }],
            host_replies: vec![
                HostCommandReply {
                    request_id: 8,
                    outcome: HostCommandOutcome::RecordingStarted {
                        actual_raw_path: "/data/run.raw".into(),
                        started_at: "2026-07-20T12:00:00Z".into(),
                    },
                },
                HostCommandReply {
                    request_id: 9,
                    outcome: HostCommandOutcome::RecordingPartial {
                        actual_raw_path: "/data/run.raw".into(),
                        size: Some(123),
                        sha256: None,
                        duration_us: 42,
                        reason: "writer failed".into(),
                    },
                },
            ],
            snapshots: vec![PluginControlSnapshot {
                plugin_id: "device.modulation".into(),
                topic: "state.v1".into(),
                revision: 3,
                payload: serde_json::json!({"connected": true}),
            }],
        };
        let encoded = serde_json::to_vec(&inbox).expect("control inbox must encode");
        assert_eq!(
            serde_json::from_slice::<PluginControlInbox>(&encoded).unwrap(),
            inbox
        );

        let request = HostCommandRequest {
            request_id: 9,
            command: HostCommand::StartRecording {
                run_id: "run-9".into(),
                base_path: "/data/run-9".into(),
                metadata: [("reference_set".into(), "ref-1".into())]
                    .into_iter()
                    .collect(),
            },
        };
        let encoded = serde_json::to_vec(&request).expect("host request must encode");
        assert_eq!(
            serde_json::from_slice::<HostCommandRequest>(&encoded).unwrap(),
            request
        );

        let service = PluginServiceRequest {
            request_id: 10,
            source_plugin_id: String::new(),
            target_plugin_id: "device.modulation".into(),
            service: "output_off.v1".into(),
            payload: serde_json::Value::Null,
        };
        let encoded = serde_json::to_vec(&service).expect("service request must encode");
        assert_eq!(
            serde_json::from_slice::<PluginServiceRequest>(&encoded).unwrap(),
            service
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
