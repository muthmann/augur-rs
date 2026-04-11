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
- `PluginCapabilities`
- `SettingsSchema` / `StatusEntry`
- `HostViewRegistry`
- `GlobalSettings`
- `TableDatasetV1`
- `Image2dV1`
- `Series1dV1`
- `CTX_GLOBAL_SETTINGS`

Domain-specific payloads live in companion crates rather than in `augur-plugin-api`. Plugins that share structured data with downstream consumers define their types in `augur-plugin-types` or their own companion crate.

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
- cached snapshots reload only when the provider reports a higher generation

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

`augur-gui` only appends decoded events into the host-owned `EventStore` when at least one enabled
plugin declares `retained_event_history: true`.

Empty-event frames are not retained.

This keeps the default preview/record path cheap when no plugin actually needs historical event
access.

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
| `augur-gui/src/plugin_loader.rs` | manifest parsing, library loading, callback bridges |
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
