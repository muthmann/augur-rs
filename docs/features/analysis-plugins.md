# Dynamic Analysis Plugin Architecture

## Summary

`augur-gui` now hosts analysis through a mixed plugin model:

- **ROI Grid** stays built in through `augur-gui/src/plugin.rs` because it mutates `CameraConfig` directly
- **scientific analysis plugins** load at runtime from `~/.augur/plugins/` through `libloading`

This keeps `augur-core` free of domain logic while removing the need to recompile `augur-gui` whenever a plugin changes.

The host now also supports a generic dataset/view registry so plugins can expose structured results
to host-rendered tables and windows without adding plugin-specific GUI code.

## Runtime Model

Each dynamic plugin ships as:

- a `plugin.toml` manifest
- one platform library (`.dylib`, `.so`, or `.dll`)
- a Rust crate that depends on `augur-plugin-api`

At startup or on rescan, `augur-gui`:

1. walks `~/.augur/plugins/`
2. parses each `plugin.toml`
3. resolves the matching library file
4. prefers the exported `augur_plugin_entry_v2` ABI descriptor when present
5. falls back to `augur_plugin_vtable` when ABI v2 is absent
6. instantiates the plugin and caches its declarative settings schema

Loader failures are non-fatal and stay visible in the Plugin Manager window.

## FFI API

The standalone `augur-plugin-api` crate defines the cross-library boundary:

- `Plugin`: safe Rust trait for plugin authors
- `export_plugin!`: exports a panic-safe C vtable
- `PluginFrame`: zero-copy access to preview pixels and optional raw events
- `HostOutput`: safe overlay and warning callbacks back into the GUI host
- `HostContext`: string-keyed context publishing and lookup using serialized JSON bytes
- `SettingsSchema` / `StatusEntry`: declarative UI and read-only status reporting
- `HostViewRegistry`: declarative dataset/view metadata for host-rendered analysis views

Shared science-facing context types live in `augur-plugin-api/src/context.rs`, including:

- `Localization`
- `LocalizationResults`
- `HostViewRegistry`
- `TableDatasetV1`
- `CTX_LOCALIZATION_RESULTS`

## Host Views

Plugins can now describe host-rendered outputs through:

- `host_views()`, which returns dataset and view descriptors
- `host_view_dataset(dataset_id)`, which returns a serialized payload on demand

The host resolves duplicate ids in plugin execution order:

- later providers win only when descriptor metadata matches exactly
- conflicting duplicate ids are ignored and logged
- dataset payloads are fetched lazily, only when a visible panel or open window needs them

Legacy plugins that export only the old vtable still load, but contribute no host views.

### Example

```rust
use augur_plugin_api::{
    HostDatasetDescriptor, HostDatasetKind, HostViewDescriptor, HostViewKind,
    HostViewPlacement, HostViewRegistry, Plugin, TableColumn, TableDatasetV1,
    TableSchema, TableValueType,
};

fn host_views(&self) -> HostViewRegistry {
    HostViewRegistry {
        datasets: vec![HostDatasetDescriptor {
            id: "molecules.table".into(),
            title: "Localized Molecules".into(),
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
                coordinate_space_2d: None,
            }),
            empty_message: "No localizations available yet.".into(),
        }],
        views: vec![
            HostViewDescriptor {
                id: "molecules.panel".into(),
                title: "Localization Preview".into(),
                dataset_id: "molecules.table".into(),
                placement: HostViewPlacement::AnalysisPanel,
                kind: HostViewKind::CompactTable,
            },
            HostViewDescriptor {
                id: "molecules.window".into(),
                title: "Localization Density".into(),
                dataset_id: "molecules.table".into(),
                placement: HostViewPlacement::Window,
                kind: HostViewKind::Density2dFromTable {
                    x_column: "x_nm".into(),
                    y_column: "y_nm".into(),
                },
            },
        ],
    }
}
```

## Execution Phases

Plugins still run in three ordered passes per preview frame:

| Phase | Use |
|---|---|
| `FrameOnly` | overlays, pixel statistics, inexpensive preview-only analysis |
| `RawEvents` | event-domain reconstruction and analysis |
| `DerivedData` | consumers of upstream plugin outputs |

Current assignments:

- `ROI Grid` — built-in `FrameOnly`
- `Hotpixel Detection` — dynamic `FrameOnly`
- `Molecule Localization` — dynamic `RawEvents`
- `EVE Candidate Finding` — dynamic `RawEvents` (in [augur-plugins](https://github.com/muthmann/augur-plugins))
- `EVE Candidate Fitting` — dynamic `DerivedData` (in [augur-plugins](https://github.com/muthmann/augur-plugins))
- `EVE Post-Processing` — dynamic `DerivedData` (in [augur-plugins](https://github.com/muthmann/augur-plugins))
- `Localization Reconstruction` — dynamic `DerivedData` (in [augur-plugins](https://github.com/muthmann/augur-plugins))
- `Focus Metrics` — dynamic `DerivedData`

## Context Bus

The old type-indexed `PluginContext` is gone for dynamic plugins. The host now stores:

```rust
HashMap<String, Vec<u8>>
```

Plugins publish and read JSON-serialized values under well-known keys. This keeps the boundary debuggable and works across independently compiled dynamic libraries.

## Settings and Status

Dynamic plugins no longer render `egui` directly. Instead they expose:

- `SettingsSchema`: sections and items describing checkboxes, sliders, drag values, and enums
- `StatusEntry`: text rows, labeled values with optional colors, and sparklines

`augur-gui/src/plugin_settings_ui.rs` turns that schema into the right-side analysis panel.

## Raw Event Transport

`PreviewFrame` still carries optional raw events. The pipeline only fills them when at least one enabled plugin declares `PluginInput::RawEvents`, so the default preview path stays cheap when event-domain plugins are disabled.

## Writing a Dynamic Plugin

1. Create a crate with `crate-type = ["cdylib", "rlib"]`
2. Implement `augur_plugin_api::Plugin`
3. Export it with `export_plugin!(MyPlugin)`
4. Add a `plugin.toml`
5. Copy the built library plus manifest into `~/.augur/plugins/<name>/`
6. Use the GUI Plugin Manager to scan and load it

The `Plugin` trait includes an optional `dependencies()` method that returns a slice of plugin name strings. The Plugin Manager uses these to show dependency relationships. If your plugin consumes an upstream context payload (such as `CTX_LOCALIZATION_RESULTS`) and cannot operate at all without that upstream producer, return its name from `dependencies()`. If your plugin degrades gracefully when the payload is absent, prefer a runtime warning over a hard dependency declaration.

See [augur-plugins](https://github.com/muthmann/augur-plugins) for the template and full contributor guide.

This workspace includes reference plugin crates under:

- `plugins/hotpixel`
- `plugins/localization`
- `plugins/focus-metrics`

## Files

| File | Role |
|---|---|
| `augur-plugin-api/src/ffi.rs` | C ABI types and vtable definition |
| `augur-plugin-api/src/helpers.rs` | safe plugin-author trait and host wrappers |
| `augur-plugin-api/src/macros.rs` | `export_plugin!` |
| `augur-gui/src/plugin_loader.rs` | manifest parsing, library loading, callback bridges |
| `augur-gui/src/plugin_settings_ui.rs` | declarative settings and status renderer |
| `augur-gui/src/plugin.rs` | built-in ROI Grid trait surface |
| `augur-gui/src/plugins/roi_grid.rs` | built-in ROI Grid implementation |
| `plugins/*/src/lib.rs` | reference dynamic plugins |

## Verification

```bash
cargo build --workspace
cargo test -p augur-plugin-api -p augur-plugin-hotpixel -p augur-plugin-localization -p augur-plugin-focus-metrics -p augur-gui
```
