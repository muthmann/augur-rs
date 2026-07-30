# Dynamic Analysis Plugin Architecture

## Summary

`augur-gui` uses a runtime-only plugin model. Plugins load from `~/.augur/plugins/`, while host-owned core tools such as hotpixel detection stay in `augur-gui` and are not plugins.

- `augur-plugin-api` owns the host/runtime contract
- `augur-plugin-types` owns optional companion payload crates for domain-specific data types
- plugin-owned GUI output flows through declarative host views instead of plugin-specific host hooks

This keeps `augur-core` free of application-specific logic and keeps `augur-gui` a general-purpose recorder, preview shell, and plugin host.

## Runtime Model

Each runtime plugin ships as:

- a `plugin.toml` manifest
- one platform library (`.dylib`, `.so`, or `.dll`)
- a Rust crate that depends on `augur-plugin-api`
- optional companion payload crates such as `augur-plugin-types`

At startup or on rescan, `augur-gui`:

1. walks `~/.augur/plugins/`
2. parses each `plugin.toml`
3. resolves the matching library file
4. loads the exported `augur_plugin_vtable` symbol
5. rejects stale plugins by comparing both `PluginVTable::vtable_size` and `PluginVTable::abi_version`
6. instantiates the plugin and caches its declarative settings/status metadata

Loader failures are non-fatal and stay visible in the Plugin Manager.

## Core API Surface

`augur-plugin-api` defines the generic host/runtime contracts:

- `Plugin`
- `export_plugin!`
- `PluginFrame`
- `HostOutput`
- `HostContext`
- `EventStoreHandle`
- `PluginDiscontinuity`
- `PluginStateKind`
- `PluginCapabilities`
- `PluginRuntimeRole`
- `PluginControlContext`
- `PluginServiceRequest` / `PluginServiceReply`
- `PluginControlSnapshot`
- `HostCommandRequest` / `HostCommandReply`
- `SettingsSchema` / `StatusEntry`
- `HostViewRegistry`
- `GlobalSettings`
- `SensorMonitoringV1` / `SensorBiasReadbackV1` / `SensorBiasCodesV1`
- `TableDatasetV1`
- `Image2dV1`
- `Series1dV1`
- `CTX_GLOBAL_SETTINGS`
- `CTX_SENSOR_MONITORING`

### Sensor measurements (`HostContext::sensor_monitoring`)

`HostContext::sensor_monitoring()` returns the absolute values the sensor
measured for the current capture: refractory period (µs), illumination (lux),
die temperature (°C), and the absolute bias codes with their per-unit factory
defaults. The camera configuration only carries relative bias offsets, so these
cannot be derived plugin-side.

```rust
if let Some(monitoring) = context.sensor_monitoring() {
    if let Some(lux) = monitoring.illumination_lux {
        // log the light level this measurement was taken under
    }
}
```

**It is optional context, never an input to results.** The value is `None` on
replay, decoded imports, and deterministic offline analysis runs — there is no
device to ask. A plugin whose output changes with its presence would disagree
between a live preview and an offline re-run of the same recording, which
breaks the guarantee that analysis runs are reproducible (ADR 025). Use it for
logging, provenance, and sanity checks on capture conditions.

Each field is independently `Option`: `None` means the sensor cannot report that
quantity. Among the biases only `refr` has a physical unit; the other four are
exposed as absolute codes because no vendor-documented conversion exists
(`docs/features/absolute-setting-values.md`).

`age_s` is how long before this frame the host actually read the sensor. The
host polls at a few hertz, so a reading is never simultaneous with its frame.

Domain-specific payloads live in companion crates rather than in `augur-plugin-api`. Plugins that share structured data with downstream consumers define their types in `augur-plugin-types` or their own companion crate.

## Frame-Independent Control (ABI v6)

Declare a stable manifest `id` before participating in control routing. The
live worker invokes `process_control` at a bounded cadence even without camera
frames. Use `PluginControlContext::request_service` for atomic, target-defined
semantic operations; implement `handle_service_request` in the device-owner
plugin and publish revisioned state through `control_snapshots`.

Do not use generic remote setting mutation for hardware protocols. A device
plugin remains the sole owner of its connection and validates complete domain
commands itself. Deduplicate or resume workflows using request IDs and target
snapshots.

The host calls `set_runtime_role` before applying copied settings. Hardware
effects are permitted only for `PluginRuntimeRole::LiveWorker` when
`ExecutionContext::hardware_effects_allowed()` is true. `UiMirror` and
`OfflineAnalysis` must stay inert.

Recording commands additionally require the corresponding manifest
`host_commands` entry. Recording paths are relative to the host-configured
output directory; absolute paths and traversal are rejected. See
[Plugin Service Control Plane](./plugin-service-control-plane.md).

## Host Views

Plugins describe host-rendered outputs through:

- `host_views()`
- `host_view_dataset(dataset_id)`
- `host_view_dataset_generation(dataset_id)`

The host resolves duplicate ids in plugin execution order:

- later providers win only when descriptor metadata matches exactly
- conflicting duplicate ids are ignored and logged
- views whose dataset ids do not resolve are ignored and logged
- views whose kinds do not match the dataset kind are ignored and logged
- dataset payloads are fetched lazily and cached by dataset id
- cached snapshots reload only when the provider reports a changed, nonzero
  generation
- providers that leave `host_view_dataset_generation` at the default `0` are
  treated as generation-less: the host reloads their datasets once per
  analysis pass instead of caching them forever — implement real generation
  counters to avoid that per-pass reload cost

Current generic dataset kinds:

- `HostDatasetKind::TableV1`
- `HostDatasetKind::Image2dV1`
- `HostDatasetKind::Series1dV1`

Current host-rendered view kinds:

- `HostViewKind::CompactTable`
- `HostViewKind::TableWindow`
- `HostViewKind::Density2dFromTable`
- `HostViewKind::Scatter2dFromTable`
- `HostViewKind::Scatter3dFromTable`
- `HostViewKind::ImageWindow`
- `HostViewKind::LineSeriesWindow`

The host owns rendering, exports, caching, and window state. Plugins do not render `egui`
directly.

## Overlay Outputs

Plugins can still emit lightweight overlay data through `HostOutput`:

- `add_highlight_pixels(...)`
- `add_crosshair_markers(...)`
- `add_marker_overlay(...)`
- `add_warning(...)`

`add_marker_overlay(...)` is the generic path for richer 2D markers. It supports:

- per-item shape: point, cross, box, ellipse, diamond, filled circle
- per-item color and size
- optional timestamp
- optional stable id
- optional overlay-level dataset id, layer id, and source label

Use host-view datasets plus metadata for the primary linked-workspace model. Use marker overlays
when the plugin needs an additional 2D annotation layer or wants 2D hit-testing on marks that do
not map cleanly to existing `HighlightPixels` / `CrosshairMarkers`.

## Event Inputs And Retained History

Plugins still declare their per-frame input phase through `input_kind()`:

- `FrameOnly`
- `RawEvents`
- `DerivedData`

Raw-event delivery and retained history are now separate concerns:

- `PluginInput::RawEvents` means the plugin needs current-frame raw events
- `PluginCapabilities { retained_event_history: true }` means the plugin needs host-retained history

When at least one enabled plugin declares `retained_event_history: true`,
`augur-gui` registers a dedicated lossless upstream cursor for runtime plugin
history. The host copies complete decoded frame windows from that cursor into
the ABI-stable `EventStoreHandle` history, so bounded preview-frame drops do
not silently remove frames from retained plugin history.

Empty-event frames are not retained.

This keeps the default preview/record path cheap when no plugin actually needs
historical event access. If the plugin-history cursor falls behind the resident
upstream ring, the host surfaces an analysis error instead of continuing with
missing retained frames.

## Lifecycle And State

The current dynamic plugin ABI is v4. Plugins built against older vtable layouts
must be rebuilt before the host will load them.

Plugins are accumulating by default. The default `on_discontinuity`
implementation calls `reset()` for accumulating plugins, which is correct for
most plugins that derive state from prior frames. Return
`PluginStateKind::Stateless` only when the plugin has no cross-frame accumulator
and every output can be recomputed from the current frame plus host context.

The host calls `on_discontinuity` on every loaded plugin — including
stateless ones, whose default implementation ignores it — when a timeline or
configuration boundary would make existing accumulated state unsafe to reuse:

- replay seek
- source/file replacement
- plugin or global setting change
- retained-history eviction after the live worker falls behind ring capacity

Do not cache `EventStoreHandle` frame ranges across `process_frame` calls or
across discontinuities. Query ranges from the handle inside the frame that needs
them.

## Example

```rust
use augur_plugin_api::{
    HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor, HostViewKind,
    HostViewPlacement, HostViewRegistry, Plugin, PluginCapabilities, TableColumn,
    TableCoordinateSpace2d, TableSchema, TableValueType,
};

fn capabilities(&self) -> PluginCapabilities {
    PluginCapabilities {
        retained_event_history: true,
    }
}

fn host_views(&self) -> HostViewRegistry {
    HostViewRegistry {
        datasets: vec![HostDatasetDescriptor {
            id: "detections.table".into(),
            title: "Detected Features".into(),
            kind: HostDatasetKind::TableV1(TableSchema {
                columns: vec![
                    TableColumn {
                        id: "frame".into(),
                        title: "Frame".into(),
                        value_type: TableValueType::U64,
                    },
                    TableColumn {
                        id: "x_px".into(),
                        title: "X [px]".into(),
                        value_type: TableValueType::F64,
                    },
                    TableColumn {
                        id: "y_px".into(),
                        title: "Y [px]".into(),
                        value_type: TableValueType::F64,
                    },
                ],
                coordinate_space_2d: Some(TableCoordinateSpace2d {
                    x_column: "x_px".into(),
                    y_column: "y_px".into(),
                    x_min: 0.0,
                    x_max: 1280.0,
                    y_min: 0.0,
                    y_max: 720.0,
                }),
            }),
            empty_message: "No detections available yet.".into(),
        }],
        views: vec![
            HostViewDescriptor {
                id: "detections.panel".into(),
                title: "Detection Preview".into(),
                dataset_id: "detections.table".into(),
                placement: HostViewPlacement::AnalysisPanel,
                kind: HostViewKind::CompactTable,
            },
            HostViewDescriptor {
                id: "detections.scatter".into(),
                title: "Detection Scatter".into(),
                dataset_id: "detections.table".into(),
                placement: HostViewPlacement::Window,
                kind: HostViewKind::Scatter2dFromTable {
                    x_column: "x_px".into(),
                    y_column: "y_px".into(),
                },
            },
            HostViewDescriptor {
                id: "detections.density".into(),
                title: "Detection Density".into(),
                dataset_id: "detections.table".into(),
                placement: HostViewPlacement::Window,
                kind: HostViewKind::Density2dFromTable {
                    x_column: "x_px".into(),
                    y_column: "y_px".into(),
                },
            },
        ],
    }
}
```

## Dependencies And Context Keys

The `Plugin` trait still has an optional `dependencies()` method that returns plugin name strings.
The Plugin Manager uses them to surface hard plugin relationships.

If a plugin consumes an upstream payload and cannot operate without it, return the producer name from `dependencies()`.

If the plugin can degrade gracefully, prefer a runtime warning over a hard dependency declaration.

## Files

| File | Role |
|---|---|
| `augur-plugin-api/src/ffi.rs` | C ABI types, `PluginCapabilities`, and flat `PluginVTable` |
| `augur-plugin-api/src/helpers.rs` | safe plugin-author trait and host wrappers |
| `augur-plugin-api/src/context.rs` | generic host datasets/views and `GlobalSettings` |
| `augur-plugin-types/src/` | optional domain-specific companion payloads |
| `augur-plugin-api/src/macros.rs` | `export_plugin!` |
| `augur-runtime/src/lib.rs` | manifest parsing, library loading, callback bridges, retained history, and live worker |
| `augur-gui/src/plugin_loader.rs` | compatibility re-export for the runtime loader |
| `augur-gui/src/host_views.rs` | registry resolution, dataset decoding, host-side rendering/export |
| `augur-gui/src/plugin_settings_ui.rs` | declarative settings and status renderer |
| `augur-gui/src/hotpixel.rs` | host-owned built-in hotpixel tool (not part of the runtime plugin ABI) |

## Verification

```bash
cargo test -p augur-plugin-api
cargo test -p augur-gui host_view
cargo test -p augur-gui event_store
cargo check -p augur-gui
```
