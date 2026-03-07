# Dynamic Analysis Plugin Architecture

## Summary

`augur-gui` now uses a mixed plugin model:

- **ROI Grid** remains built in because it edits `CameraConfig`
- scientific plugins load at runtime from `~/.augur/plugins/`

This keeps the host application stable while allowing plugin crates to be rebuilt and reloaded independently.

## Main Pieces

- `augur-plugin-api`: FFI types, shared context models, declarative settings/status schema, and the `export_plugin!` macro
- `augur-gui/src/plugin_loader.rs`: manifest parsing, library loading, and callback bridging
- `augur-gui/src/plugin_settings_ui.rs`: renders plugin settings and status entries in `egui`
- `plugins/*`: reference dynamic plugin crates in this workspace

## Execution Model

Plugins still run in ordered phases:

- `FrameOnly`
- `RawEvents`
- `DerivedData`

The preview pipeline only transports raw events when an enabled plugin requests them.

## Context Bus

Dynamic plugins exchange derived data through a per-frame `HashMap<String, Vec<u8>>` filled with JSON payloads. Shared keys and types, such as localization results, live in `augur-plugin-api`.
