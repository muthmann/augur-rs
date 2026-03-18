# Technical Notes

These notes capture feature-level behavior and implementation constraints useful for maintainers and advanced users.

## Plugin Ecosystem

This repository defines the plugin API, runtime loader, and a small set of reference dynamic plugin crates. The long-term home for the broader plugin registry remains **[augur-plugins](https://github.com/muthmann/augur-plugins)**.

See [Plugin Architecture](./analysis-plugins.md) for the interface spec and [Dynamic Plugin Loading](./dynamic-plugins.md) for the install/reload workflow.

---

## Index

- [AugurRS SDK Overview](./augur-sdk.md) — shared backend, runtime guarantees, IMX636-specific behavior, recording details
- [Release Distribution](./release-distribution.md) — cross-platform tagged artifacts, macOS signing/notarization, and cargo-release workflow
- [HDF5 File Support](./hdf5-file-support.md) — native HDF5 build requirements, ECF plugin installation, runtime environment setup
- [Replay](./raw-replay.md) — `.raw` plus decoded-event replay, seekable transport controls, persisted EOF state
- [Theme-Aware GUI Status Colors](./theme-aware-gui-status-colors.md) — theme-adaptive warning/error labels plus contrast-safe success/info colors
- [Collapsible GUI Panels](./collapsible-panels.md) — separator-edge collapse arrows plus scrollable settings and analysis panels
- [Interactive Preview Workbench](./interactive-preview-workbench.md) — shared 2D/3D preview state, ROI drag tool, zoom/crop, popup enlarge, point cloud
- [Plugin Architecture](./analysis-plugins.md) — mixed built-in/runtime plugin host, FFI API, phased execution, shared context types
- [Host View Registry](./host-view-registry.md) — generic dataset/view descriptors, precedence rules, host-rendered tables and density views
- [Dynamic Plugin Loading](./dynamic-plugins.md) — plugin directory layout, manifest expectations, and Plugin Manager workflow
- [ROI Grid Computation](./roi-grid.md) — grid partitioning around masked hotpixels and maximal-rectangle selection
- [Output File Timestamps](./output-file-timestamps.md) — overwrite protection and automatic timestamp suffixes for recordings

The following notes document plugins that ship in [augur-plugins](https://github.com/muthmann/augur-plugins):

- [Molecule Localization Plugin](./molecule-localization.md) — wavelet denoising, Gaussian fitting, localization result publishing
- [Focus Metrics Plugin](./focus-metrics.md) — localization-based and FFT-based focus estimation
