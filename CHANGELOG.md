# Changelog

## Unreleased

### ⚠ BREAKING CHANGES

* **plugin-api:** `FfiMarkerOverlayItem` gains `source_dataset_id` and `source_row_id` fields and `PLUGIN_ABI_VERSION` bumps from 3 to 4. Runtime plugins must rebuild against current `augur-plugin-api`.

### Features

* ✨ clickable 2D overlays bound to their backing row via marker `source_row`, with reason-first summary card for rejected-fit rows.
* ✨ generic host action bus: plugins declare `HostActionDescriptor`s with `Dataset`/`Row`/`Cluster` scope and an optional `param_schema`; the host renders scope-aware buttons plus a schema-driven modal and publishes `HostActionRequest`s to `CTX_INVESTIGATION_ACTION_REQUESTS`.

## [1.0.0](https://github.com/muthmann/augur-rs/compare/v0.1.0...v1.0.0) (2026-04-07)


### ⚠ BREAKING CHANGES

* **pipeline:** PluginVTable gains host_view_dataset_generation and FfiEventStoreHandle switches from contiguous slices to frame-based callbacks. Runtime plugins must rebuild against current augur-plugin-api.
* **plugin-api:** PluginVTable layout changed, process_frame takes a 5th FfiEventStoreHandle parameter, FfiPluginContext has two new fields, and the deprecated accumulated_localizations trait method is removed.

### Features

* ✨ add generic host view registry ([36b6288](https://github.com/muthmann/augur-rs/commit/36b628880f0227e6cc1cc52da3365e842cc33630))
* ✨ add generic host view registry ([a5fa910](https://github.com/muthmann/augur-rs/commit/a5fa91063294cf19fb5c967cb3dae8def51120f3))
* ✨ add ImageJ timeline stack history ([5606b58](https://github.com/muthmann/augur-rs/commit/5606b58198e75eb2ec5b37bd56909c9e1cd8c84a))
* ✨ add reconstruction window and exports ([f71e14f](https://github.com/muthmann/augur-rs/commit/f71e14f331b645d2d451c273d1cb3a6552526af0))
* ✨ add rich recording metadata ([3451328](https://github.com/muthmann/augur-rs/commit/34513282cb042a6025b0ab8529f579fd6da01064))
* ✨ extract reusable viewer widget ([a720f3d](https://github.com/muthmann/augur-rs/commit/a720f3dbeb45dc1451a456f56ddd2e542437975d))
* ✨ redesign replay timing model ([5dde7c6](https://github.com/muthmann/augur-rs/commit/5dde7c6deef591f917f12aa20a77de710fdfb0a9))
* **gui:** ✨ add global settings menu, plugin GlobalSettings context, and fix replay speed ([7ddcb65](https://github.com/muthmann/augur-rs/commit/7ddcb6537fd153d8311ab4ba0810793bb6f01649))
* **gui:** ✨ add interactive viewer tools, colormaps, and ImageJ bridge ([efef941](https://github.com/muthmann/augur-rs/commit/efef94162baef6a111b1aa6294543ce0ef57a274))
* **gui:** ✨ add official ImageJ LUTs and paused preview refresh ([d807468](https://github.com/muthmann/augur-rs/commit/d807468505410a4d262f5bebb5009f28c183a282))
* **gui:** ✨ add PreviewMode enum with polarity, signed count, time surface, and Hz settings ([3f73bf5](https://github.com/muthmann/augur-rs/commit/3f73bf5e32ed4fa3a3b93aa09790ac18e1121fb6))
* **gui:** ✨ add TIFF stack export and replay-editable acquisition time ([8626014](https://github.com/muthmann/augur-rs/commit/862601400a183b30384e78ab5b4502f36088611b))
* **gui:** ✨ add TIFF stack export and replay-editable acquisition time ([45016c8](https://github.com/muthmann/augur-rs/commit/45016c8544be768e43a235dd6208e62e3a04b779))
* **gui:** ✨ binary frame protocol for flicker-free ImageJ streaming ([5181046](https://github.com/muthmann/augur-rs/commit/5181046fd2ba6c4da8c580f55c3f3cbd681cec47))
* **gui:** add global settings menu with plugin context and replay speed fix ([c769722](https://github.com/muthmann/augur-rs/commit/c769722eeef26c045c4ed7f33238e490cf2672ae))
* **gui:** interactive viewer tools, ImageJ bridge, and event camera visualization modes ([4278130](https://github.com/muthmann/augur-rs/commit/42781303801d1e8b4eb0fae2fbfd827a2147fc4c))
* **plugin-api:** unified VTable, EventStore, and v0.2 cleanup ([7e11fc9](https://github.com/muthmann/augur-rs/commit/7e11fc98a86379c3d46b5e04e73d3faa6b8a1cc6))


### Bug Fixes

* 🐛 add bundled macOS app icon ([a7d8658](https://github.com/muthmann/augur-rs/commit/a7d865813ce4cdc659e4b8a0b59ce7033a4e13e2))
* 🐛 align replay controls and add theme shortcuts ([f123c61](https://github.com/muthmann/augur-rs/commit/f123c61484b62fd41d570debc6035556af5ebf8d))
* 🐛 decouple raw replay pacing from preview drops ([4c95fd0](https://github.com/muthmann/augur-rs/commit/4c95fd0ea0b5f57c76ed9ec92e0cc45748424b70))
* 🐛 harden plugin loading and rename the GUI to AugurRS ([d0dfb39](https://github.com/muthmann/augur-rs/commit/d0dfb39d177de5b3121ae728127cf24eeaa1c3d0))
* 🐛 harden raw replay stepping and header fallbacks ([d35cff2](https://github.com/muthmann/augur-rs/commit/d35cff2997b19839c1cc4b00b715df8b371af694))
* 🐛 harden raw replay stepping and header fallbacks ([f667efa](https://github.com/muthmann/augur-rs/commit/f667efae9cdbfda1e27a975480cd80432a7152aa))
* 🐛 restore modern ImageJ bridge workflow ([4931b78](https://github.com/muthmann/augur-rs/commit/4931b787a64896aceb530821ecba522ffb6172df))
* 🐛 stabilize replay seek and frame stepping ([83ea6b2](https://github.com/muthmann/augur-rs/commit/83ea6b2bf43e94af77e4bee2da431c07d4d57599))
* **clippy:** 🧹 resolve default-constructed-unit-struct and field-reassign-with-default warnings ([9d1ed54](https://github.com/muthmann/augur-rs/commit/9d1ed545ed7ebc67b2f11cad7d00c67f6c834771))
* **gui:** 🎨 default preview to summed intensity (Grays) instead of polarity ([d15e4e9](https://github.com/muthmann/augur-rs/commit/d15e4e93d64d17aed530eabbbea24cb4896f39b4))
* **gui:** 🎨 improve ImageJ bridge dialog UX and rename plugin jar ([f22c7cb](https://github.com/muthmann/augur-rs/commit/f22c7cbde718baf4db771408a05d46ecf58bb98e))
* **gui:** 🐛 clip preview painters to panel bounds ([f221282](https://github.com/muthmann/augur-rs/commit/f2212829449f8ccf7dd72270ef1d3625928b023e))
* **gui:** 🐛 fix ImageJ dialog text overlap and broken Unicode glyphs ([d46b8b3](https://github.com/muthmann/augur-rs/commit/d46b8b354d415642758a57539125ed889083b0e2))
* **gui:** 🐛 fix plugin radio and drag settings never persisting ([8bb3a8f](https://github.com/muthmann/augur-rs/commit/8bb3a8ff7851bd64b4fb683a56245816e622bc4c))
* **gui:** 🐛 improve ROI editing and IMX636 scale defaults ([9a38de3](https://github.com/muthmann/augur-rs/commit/9a38de3f2e1a4af121d6aea6d95d1ffb7df93c43))
* **gui:** 🐛 make time-surface hover readout mode-aware ([4c2fd7a](https://github.com/muthmann/augur-rs/commit/4c2fd7aa6626a3d9c55098dc692581edc7c762d5))
* **gui:** 🐛 restore underscore suffix for ImageJ plugin discovery ([f71c518](https://github.com/muthmann/augur-rs/commit/f71c5182b47b07552c0cda177ca05cf0dda8a765))
* **gui:** 🐛 stabilize deferred replay and reconstruction windows ([cda5908](https://github.com/muthmann/augur-rs/commit/cda5908b7ffffd3ae2d1b72c7ee1853f7c6408a0))
* **gui:** 🐛 truncate hover stats when toolbar space is tight ([23b99c1](https://github.com/muthmann/augur-rs/commit/23b99c10ec0bb90302606622231001cbf425b675))
* **gui:** 🐛 use horizontal_wrapped for preview toolbar to prevent overflow ([cb38344](https://github.com/muthmann/augur-rs/commit/cb383440727c0602729271af5e469d0a4f92b3b2))
* **gui:** improve ROI editing and IMX636 scale defaults ([2583b19](https://github.com/muthmann/augur-rs/commit/2583b1956ced058b5f575ce84a88beedbbc5a0d5))
* **gui:** make time-surface hover readout mode-aware ([e808532](https://github.com/muthmann/augur-rs/commit/e808532647a1df9978bcd63c893121ceb01f1354))
* harden plugin loading and rename the GUI to AugurRS ([66a10c4](https://github.com/muthmann/augur-rs/commit/66a10c41df21c5a73c6413f423627e4cb28b4c99))
* macOS app icon packaging and sizing ([373abb9](https://github.com/muthmann/augur-rs/commit/373abb92c5bbafe08926f040ec1492ad6915d3e3))
* **plugin-api:** add vtable_size guard to prevent SIGILL on stale plugins ([4d88bd2](https://github.com/muthmann/augur-rs/commit/4d88bd209c8796a8a6beb2db7f5a00851656d936))


### Performance Improvements

* 🚀 add preview histogram caches and thread timings ([0781276](https://github.com/muthmann/augur-rs/commit/07812766995c81ece0ff617a0f3e0c68645705dd))
* 🚀 add wgpu preview rendering path ([5f1cd88](https://github.com/muthmann/augur-rs/commit/5f1cd8897ee5b0e1dcc03449839985b8bd5a4542))
* 🚀 move time-surface preview fully onto gpu ([e719c03](https://github.com/muthmann/augur-rs/commit/e719c03db44cee47080ecea6b143958fbe53e3d9))
* **gui:** ✨ optimize host view registry for per-frame efficiency ([9409937](https://github.com/muthmann/augur-rs/commit/9409937960c57638c41a5338b16e8e198a80ba14))
* **gui:** 🚀 simplify preview hot paths and ImageJ streaming ([9ad4630](https://github.com/muthmann/augur-rs/commit/9ad4630e1899b616d5f69edd82f4fd12402c4287))
* **pipeline:** ⚡ eliminate allocation hotspots in capture and preview paths ([c2a5887](https://github.com/muthmann/augur-rs/commit/c2a588740a4c9f192b2b6e493e955f0e4c236064))
* **pipeline:** ⚡ segmented EventStore, buffer pooling, direct Color32 rendering, and pipeline telemetry ([0aaf932](https://github.com/muthmann/augur-rs/commit/0aaf9327e177a0bdcafae229cec4f9181e32054a))
* **pipeline:** eliminate GUI performance hotspots ([8677ad4](https://github.com/muthmann/augur-rs/commit/8677ad40ec6efc97c8f38c2015f51b004e1d7d9d))

## [Unreleased]
