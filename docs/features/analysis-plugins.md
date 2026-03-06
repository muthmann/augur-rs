# GUI Analysis Plugin Architecture

## Repository Split

The **plugin API and runtime** are defined here in `augur-rs`.
**Plugin implementations** — including the scientific analysis suite — live in the companion repository:

> **[augur-plugins](https://github.com/muthmann/augur-plugins)** — plugin registry and implementations

This separation keeps `augur-rs` a general-purpose camera tool. Plugins are opt-in and domain-specific. Adding or removing a plugin is a one-line registration change in `augur-gui/src/plugins/mod.rs`.

---

## Summary

`augur-gui` hosts preview-side analysis through a compile-time plugin layer. Plugins share typed derived data through a `PluginContext`, declare the kind of input they require, and run in ordered phases so downstream plugins can consume upstream results in the same preview frame.

Design goals:

- `augur-core` remains a camera SDK and streaming pipeline with no domain-specific knowledge
- all analysis logic lives in plugin files in `augur-gui` (or in external plugin repos)
- removing plugin files and their registration lines returns the app to a plain recording tool

---

## Plugin API

### `AnalysisPlugin` Trait

Each plugin implements one trait covering:

- enable / disable state
- settings UI (rendered in the Analysis panel)
- per-frame processing
- reset behavior
- optional context-aware processing
- declared dependencies and input kind

Extension points are backward-compatible defaults:

- `process_frame_with_context(frame, context, settings)`
- `input_kind() -> PluginInput`
- `dependencies() -> &[&str]`

### `PluginContext` — Typed Data Bus

`PluginContext` is a type-indexed data bus shared across all plugins for the current frame.

| Method | Description |
|---|---|
| `publish::<T>(value)` | Store a derived value under its type |
| `get::<T>()` | Retrieve a value type-safely |
| `raw_events` | Raw `CdEvent` stream for the current preview window |

### Execution Phases

Plugins run in three ordered passes per preview frame:

| Phase | When it runs | Typical use |
|---|---|---|
| `FrameOnly` | First | Overlays, pixel statistics — cheap, no event reconstruction |
| `RawEvents` | Second | Plugins that need the raw event stream |
| `DerivedData` | Third | Plugins that consume results from earlier phases |

Current phase assignments (in `augur-plugins`):

- `Hotpixel Detection` — `FrameOnly`
- `ROI Grid` — `FrameOnly`
- `Molecule Localization` — `RawEvents`
- `Focus Metrics` — `DerivedData`

### Raw Event Transport

`PreviewFrame` carries an optional `events: Option<Vec<CdEvent>>`. The pipeline only fills this field when a running plugin declares `PluginInput::RawEvents`, controlled by `PipelineController::raw_events_needed`. When no plugin needs raw events, the preview path stays on its low-overhead default.

---

## Shared Plugin Types

Science-facing shared types live in `augur-gui/src/plugins/types.rs`, not in `augur-core`.

Currently:

- `Localization`
- `LocalizationResults`

These are consumed by downstream plugins (e.g. Focus Metrics reads `LocalizationResults` published by Molecule Localization) while the SDK crate remains free of domain-specific data models.

---

## Writing a Plugin

1. Implement the `AnalysisPlugin` trait in a new file under `augur-gui/src/plugins/`
2. Register it in `augur-gui/src/plugins/mod.rs`
3. Declare the input kind and any dependencies
4. Publish results to `PluginContext` if downstream plugins should consume them

To contribute a plugin to the shared ecosystem, open a PR at [augur-plugins](https://github.com/muthmann/augur-plugins).

---

## Files (this repository)

| File | Role |
|---|---|
| `augur-gui/src/plugin.rs` | `AnalysisPlugin` trait, `PluginInput`, `PluginContext` |
| `augur-gui/src/plugins/mod.rs` | Plugin registry |
| `augur-gui/src/plugins/types.rs` | Shared plugin-layer types (`Localization`, `LocalizationResults`) |
| `augur-gui/src/app.rs` | Plugin host, phase execution, dependency display |
| `augur-core/src/pipeline.rs` | Optional raw-event transport on preview frames |

---

## Verification

```bash
cargo build
cargo test -p augur-core
```
