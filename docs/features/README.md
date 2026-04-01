# Technical Notes

These notes capture feature-level behavior and implementation constraints useful for maintainers and advanced users.

## Plugin Ecosystem

This repository defines the plugin API, runtime loader, host-side analysis UI, and the built-in
ROI-grid tool. The broader runtime plugin registry lives in
**[augur-plugins](https://github.com/muthmann/augur-plugins)**.

See [Plugin Architecture](./analysis-plugins.md) for the interface spec and [Dynamic Plugin Loading](./dynamic-plugins.md) for the install/reload workflow.

---

## Index

- [AugurRS SDK Overview](./augur-sdk.md) — shared backend, runtime guarantees, IMX636-specific behavior, recording details
- [Performance Safeguards](./performance-safeguards.md) — capture-path fail-fast setup, stop-time drain behavior, preview throttling, and UI-side load shedding
- [Performance Overhaul](./performance-overhaul.md) — split pipeline telemetry, direct `Color32` preview rendering, async decoded replay opening, and generation-aware host caching
- [Global Settings Menu and Replay Pacing](./global-settings-menu.md) — top-bar settings controls, hover/reference guidance, persisted `[global]` config, plugin `GlobalSettings`, replay-editable acquisition time, and speed-change replay resets
- [TIFF Stack Export](./tiff-stack-export.md) — File-menu batch export of replay accumulations to multi-page 16-bit TIFF with time-window and ROI controls
- [Release Distribution](./release-distribution.md) — cross-platform tagged artifacts, macOS signing/notarization, and cargo-release workflow
- [HDF5 File Support](./hdf5-file-support.md) — native HDF5 build requirements, ECF plugin installation, runtime environment setup
- [Replay](./raw-replay.md) — `.raw` plus decoded-event replay, seekable transport controls, persisted EOF state
- [Theme-Aware GUI Status Colors](./theme-aware-gui-status-colors.md) — theme-adaptive warning/error labels plus contrast-safe success/info colors
- [Collapsible GUI Panels](./collapsible-panels.md) — separator-edge collapse arrows plus scrollable settings and analysis panels
- [Interactive Preview Workbench](./interactive-preview-workbench.md) — shared 2D/3D preview state, idle launch shortcuts, ROI drag tool, zoom/crop, popup enlarge, point cloud
- [Viewer Tools And ImageJ Bridge](./viewer-tools-and-imagej.md) — host-side histogram/colormap/measurement tools plus the bundled Augur Bridge handoff path for ImageJ/Fiji
- [Reusable Viewer Widget](./reusable-viewer-widget.md) — one shared central viewer component for the main window, popup host, and future reconstruction wrapping
- [Plugin Architecture](./analysis-plugins.md) — mixed built-in/runtime plugin host, FFI API, phased execution, shared context types
- [Plugin API v0.2](./plugin-api-v0-2.md) — flat runtime ABI, EventStore history, persistent plugin context, and repo split cleanup
- [Plugin API v0.3 Migration](./plugin-api-v0-3-migration.md) — segmented EventStore access, host-view dataset generations, raw context publishing, and rebuild requirements
- [Host View Registry](./host-view-registry.md) — generic dataset/view descriptors, precedence rules, host-rendered tables and density views
- [Dynamic Plugin Loading](./dynamic-plugins.md) — plugin directory layout, manifest expectations, and Plugin Manager workflow
- [ROI Grid Computation](./roi-grid.md) — grid partitioning around masked hotpixels and maximal-rectangle selection
- [Output File Timestamps](./output-file-timestamps.md) — overwrite protection and automatic timestamp suffixes for recordings

The following notes document plugins that ship in [augur-plugins](https://github.com/muthmann/augur-plugins):

- [Molecule Localization Plugin](./molecule-localization.md) — wavelet denoising, Gaussian fitting, localization result publishing
- [Focus Metrics Plugin](./focus-metrics.md) — localization-based and FFT-based focus estimation
