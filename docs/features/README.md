# Technical Notes

These notes capture feature-level behavior and implementation constraints useful for maintainers and advanced users.

## Plugin Ecosystem

This repository defines the plugin API, runtime loader, and a small set of reference dynamic plugin crates. The long-term home for the broader plugin registry remains **[augur-plugins](https://github.com/muthmann/augur-plugins)**.

See [Plugin Architecture](./analysis-plugins.md) for the interface spec and [Dynamic Plugin Loading](./dynamic-plugins.md) for the install/reload workflow.

---

## Index

- [AugurRS SDK Overview](./augur-sdk.md) — shared backend, runtime guarantees, IMX636-specific behavior, recording details
- [Replay](./raw-replay.md) — `.raw` plus decoded-event replay, seekable transport controls, persisted EOF state
- [Collapsible GUI Panels](./collapsible-panels.md) — toolbar toggles for the settings and analysis side panels
- [Plugin Architecture](./analysis-plugins.md) — mixed built-in/runtime plugin host, FFI API, phased execution, shared context types
- [Dynamic Plugin Loading](./dynamic-plugins.md) — plugin directory layout, manifest expectations, and Plugin Manager workflow
- [ROI Grid Computation](./roi-grid.md) — grid partitioning around masked hotpixels and maximal-rectangle selection

The following notes document plugins that ship in [augur-plugins](https://github.com/muthmann/augur-plugins):

- [Molecule Localization Plugin](./molecule-localization.md) — wavelet denoising, Gaussian fitting, localization result publishing
- [Focus Metrics Plugin](./focus-metrics.md) — localization-based and FFT-based focus estimation
