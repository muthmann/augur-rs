# Technical Notes

These notes capture feature-level behavior and implementation constraints useful for maintainers and advanced users.

## Plugin Ecosystem

Plugin implementations are maintained in the companion repository:
**[augur-plugins](https://github.com/muthmann/augur-plugins)** — plugin registry, implementations, and contribution guide.

This repository defines the plugin API and runtime. See [Plugin Architecture](./analysis-plugins.md) for the interface spec.

---

## Index

- [AugurRS SDK Overview](./augur-sdk.md) — shared backend, runtime guarantees, IMX636-specific behavior, recording details
- [RAW Replay](./raw-replay.md) — fast-open file replay, seekable transport controls, persisted EOF state
- [Collapsible GUI Panels](./collapsible-panels.md) — toolbar toggles for the settings and analysis side panels
- [Plugin Architecture](./analysis-plugins.md) — compile-time plugin model, typed plugin context, phased execution, raw-event transport
- [ROI Grid Computation](./roi-grid.md) — grid partitioning around masked hotpixels and maximal-rectangle selection

The following notes document plugins that ship in [augur-plugins](https://github.com/muthmann/augur-plugins):

- [Molecule Localization Plugin](./molecule-localization.md) — wavelet denoising, Gaussian fitting, localization result publishing
- [Focus Metrics Plugin](./focus-metrics.md) — localization-based and FFT-based focus estimation
