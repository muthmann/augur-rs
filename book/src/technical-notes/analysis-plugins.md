# GUI Analysis Plugin Architecture

## Summary

`augur-gui` uses a compile-time plugin system for preview-side analysis tools. Plugins now share typed derived data through a `PluginContext`, declare their input needs, and run in ordered phases so downstream tools can consume upstream results in the same frame.

## Why

The plugin model separates recording and camera control from scientific analysis tooling.

It also preserves the repository's layering rule:

- `augur-core` remains the SDK and pipeline layer
- localization and focus logic live in GUI plugin files
- removing plugin files and their registration lines returns the app to a plain recorder

## Main Pieces

- `AnalysisPlugin`: plugin trait with defaults for context-aware processing and dependency declaration
- `PluginContext`: type-indexed per-frame data exchange
- `PluginInput`: phase selection for `FrameOnly`, `RawEvents`, and `DerivedData`
- optional raw-event transport on `PreviewFrame`

## Current Plugins

- hotpixel detection
- ROI grid
- molecule localization
- focus metrics
