# Changelog

## Unreleased

### ⚠ BREAKING CHANGES

* **plugin-api:** `FfiMarkerOverlayItem` gains `source_dataset_id` and `source_row_id` fields and `PLUGIN_ABI_VERSION` bumps from 3 to 4. Runtime plugins must rebuild against current `augur-plugin-api`.

### Features

* ✨ clickable 2D overlays bound to their backing row via marker `source_row`, with reason-first summary card for rejected-fit rows.
* ✨ generic host action bus: plugins declare `HostActionDescriptor`s with `Dataset`/`Row`/`Cluster` scope and an optional `param_schema`; the host renders scope-aware buttons plus a schema-driven modal and publishes `HostActionRequest`s to `CTX_INVESTIGATION_ACTION_REQUESTS`.

## [2.1.0](https://github.com/muthmann/augur-rs/compare/v2.0.2...v2.1.0) (2026-08-14)


### Features

* **camera:** ✨ let plugins apply confirmed profiles ([4dc1111](https://github.com/muthmann/augur-rs/commit/4dc11119d46041bbffcb7f181386400d1b4897a6))
* **camera:** ✨ state the ERC setting in camera snapshots ([5d371e2](https://github.com/muthmann/augur-rs/commit/5d371e20ed80a6d7dd34713321a8d4d1e4f05fb6))
* **camera:** add confirmed plugin configuration sessions ([6793ef6](https://github.com/muthmann/augur-rs/commit/6793ef624b20dd26059ece8a3f62f2a1af1881ab))
* **runtime:** ✨ gate plugin loading on a minimum host version ([6f94093](https://github.com/muthmann/augur-rs/commit/6f94093dbb6cd23a0663ff943e875834f677df47))


### Bug Fixes

* **build:** 🐛 re-sign the macOS app bundle after packaging ([138393c](https://github.com/muthmann/augur-rs/commit/138393c5cbec119d39c216610592865f092b10ee))
* **gui:** 🐛 gate profile imports by platform ([84c8074](https://github.com/muthmann/augur-rs/commit/84c8074da597a8ffeadb851a79d8d5c2a17a8fa5))
* **gui:** 🐛 replace camera profiles atomically on Windows ([887f535](https://github.com/muthmann/augur-rs/commit/887f53544d8dbfa7f0634d920c959f78c670c572))
* **gui:** 🐛 stop a plugin card header widening the analysis panel ([51939e0](https://github.com/muthmann/augur-rs/commit/51939e0de3b3166b5ce5620ea5fa226bd5edab20))
* **gui:** 🐛 stop a plugin card header widening the analysis panel ([b257e14](https://github.com/muthmann/augur-rs/commit/b257e14e667b58d395db5a5afd4ea77988590d52))
* **runtime:** 🐛 harden standalone configuration workflows ([3f0ea1e](https://github.com/muthmann/augur-rs/commit/3f0ea1ec5441d5a8e17fc25075637f4c9df7740a))

## [2.0.2](https://github.com/muthmann/augur-rs/compare/v2.0.1...v2.0.2) (2026-08-07)


### Bug Fixes

* **gui:** 🐛 stop a narrow ROI panicking the 2D/3D split divider ([ff46629](https://github.com/muthmann/augur-rs/commit/ff46629147e2a114526d1d041ce29edbf5962c44))
* **gui:** 🐛 stop a narrow ROI panicking the 2D/3D split divider ([a6539b7](https://github.com/muthmann/augur-rs/commit/a6539b7d63c2e0627e4a2a74d3d5817fb6dfda43))

## [2.0.1](https://github.com/muthmann/augur-rs/compare/v2.0.0...v2.0.1) (2026-08-05)


### Bug Fixes

* **gui:** 🐛 size preview dispatches against the device limit ([7d98883](https://github.com/muthmann/augur-rs/commit/7d98883ed5406077d2c44cf241ee5c5f1deb2743))

## [2.0.0](https://github.com/muthmann/augur-rs/compare/v1.0.0...v2.0.0) (2026-08-05)


### ⚠ BREAKING CHANGES

* **plugin-api:** FfiMarkerOverlayItem gains source_dataset_id and source_row_id; PLUGIN_ABI_VERSION bumps from 3 to 4. Runtime plugins must rebuild against current augur-plugin-api.

### Features

* ✨ add host-owned investigation workspace ([2933536](https://github.com/muthmann/augur-rs/commit/2933536130f1657fcbdc062f936e2064caf356ae))
* ✨ implement analysis execution model ([2d82e84](https://github.com/muthmann/augur-rs/commit/2d82e84c110a84944f249cec1665e0d6e5175595))
* ✨ reorganize viewer toolbar and inspector layout ([9fac2e7](https://github.com/muthmann/augur-rs/commit/9fac2e7ea42e851c43a1831d680c3373a7c4ae8a))
* **api,gui:** ✨ add Text, Path, and Button setting kinds ([ec16089](https://github.com/muthmann/augur-rs/commit/ec16089d601ef6fc5c474879b09375f0d207b706))
* **api,runtime,gui:** ✨ add worker-owned plugin control plane ([85cc03c](https://github.com/muthmann/augur-rs/commit/85cc03cfbb2ef0276cc47b958d986803a9b36225))
* **cli:** ✨ add analyze time-range overrides, fix inverted cancel flag ([1b3b632](https://github.com/muthmann/augur-rs/commit/1b3b6329e312cb7bfb4b2657d5b048d59d4795bc))
* **core,api,runtime:** ✨ deliver external triggers and execution context to plugins (ABI v5) ([737b9d0](https://github.com/muthmann/augur-rs/commit/737b9d000e46e49fc5f8bae8d719fa38f0748ab3))
* **core,prophesee,gui,cli:** ✨ show absolute values for abstract camera settings ([14f3e45](https://github.com/muthmann/augur-rs/commit/14f3e451562da6e7cf054ba22b4c187ae2f0b6f0))
* **core:** ✨ split camera control from stream reads to keep recording gap-free ([83329af](https://github.com/muthmann/augur-rs/commit/83329af8da52b1e0a02926c586f4a8f75086406b))
* **gui,cli:** ✨ check for and install updates from inside the app ([3cf5567](https://github.com/muthmann/augur-rs/commit/3cf5567f7c05334a7c10c5a68f78c998e916e5db))
* **gui:** ✨ add measurement cursors and visible-window statistics to series views ([e16e4cf](https://github.com/muthmann/augur-rs/commit/e16e4cfc800a4c15850c58f09785783c407e1670))
* **gui:** ✨ add per-view and global freeze for host-view snapshots ([25b93c1](https://github.com/muthmann/augur-rs/commit/25b93c1ca12d00bccaf0792405e3b29737a0522d))
* **gui:** ✨ add PNG export to series host-view windows ([88cdd6c](https://github.com/muthmann/augur-rs/commit/88cdd6c1bd5ca335d97749891aaa3c70d2970b62))
* **gui:** ✨ leave evidence when a session dies ([4cd35ad](https://github.com/muthmann/augur-rs/commit/4cd35ad0a7c25fbb611afea38a82aa59cf8855aa))
* **gui:** ✨ make analysis runs the primary analysis workflow ([5915726](https://github.com/muthmann/augur-rs/commit/5915726c24be6b009b1d0de890e7b2a7ffe6c537))
* **gui:** 🎨 implement AugurRS design system (passes 1–6.2) ([786e445](https://github.com/muthmann/augur-rs/commit/786e445ac944e534c44a8343fbb510c1c5cbd29b))
* **gui:** add Python event ingress module and documentation ([8c0ec93](https://github.com/muthmann/augur-rs/commit/8c0ec93587d2feeee866cfb51e757390c441f696))
* **gui:** complete AugurRS design system (passes 6–8 + toast integration) ([ecf293d](https://github.com/muthmann/augur-rs/commit/ecf293d54bda5a1f8fbc8c1c5b4cf4f8e881f9dc))
* **plugin-api,runtime,gui:** ✨ expose sensor measurements to plugins ([29741c0](https://github.com/muthmann/augur-rs/commit/29741c0e8040fc1f803f62785b9ba1e483dd4200))
* **plugin-api:** ✨ add host action bus and clickable overlay row binding ([e9c0974](https://github.com/muthmann/augur-rs/commit/e9c0974a2820c5d451dbee675e41ac48bfdf91d6))
* **runtime:** ✨ add half-open analysis ranges, replay probing, and a JSON→TOML settings bridge ([1fccfa0](https://github.com/muthmann/augur-rs/commit/1fccfa005b2d675ee54c24e84089a5f88a921b44))
* **update:** ✨ add the augur-update crate ([803d4ba](https://github.com/muthmann/augur-rs/commit/803d4bad4210e1d2207400847a90e4c461e14cbb))


### Bug Fixes

* 🐛 apply replay acquisition time to emitted frame windows ([05f49a5](https://github.com/muthmann/augur-rs/commit/05f49a55d34ede5d9f83b7e4cd0e3b55cb2d9072))
* 🐛 restore investigation candidate click selection ([bad2acd](https://github.com/muthmann/augur-rs/commit/bad2acdee5d9edb9badf5e28db66bb556ea85013))
* 🐛 route plugin history through upstream event source ([5cda19c](https://github.com/muthmann/augur-rs/commit/5cda19c95fe680fcf0f555daa4a19151fc6274e8))
* **ci:** 🐛 grant the packaging caller the permissions release.yml declares ([a1fe088](https://github.com/muthmann/augur-rs/commit/a1fe08895ffcd4d353bd50609d9f965b7a4fee06))
* **ci:** 🐛 make tagged releases actually publish binaries ([2b447c6](https://github.com/muthmann/augur-rs/commit/2b447c6ea1a23d29334339d903141bbc97a3c021))
* **ci:** update Cargo.lock after v1.0.0 release ([654eac5](https://github.com/muthmann/augur-rs/commit/654eac554e2533947fd52c3ba374f25e0b42de01))
* **ci:** update Cargo.lock after v1.0.0 release and auto-sync on future releases ([8cb1d12](https://github.com/muthmann/augur-rs/commit/8cb1d12b2d3296a51692210f63c6811377078588))
* **core,prophesee,gui,cli:** 🐛 close remaining .raw recording gap sources ([4e286a3](https://github.com/muthmann/augur-rs/commit/4e286a3ccfc81a323096cf16ae4f0c2f56281050))
* **core:** 🐛 close lossless-recording gaps in the disk path ([e8eac94](https://github.com/muthmann/augur-rs/commit/e8eac94bb7b67d8ec6ff5798646f4722ca64bbac))
* **core:** 🐛 keep preview alive when a frame exceeds event-ring capacity ([65e1906](https://github.com/muthmann/augur-rs/commit/65e1906fce59ed5c3dc7e614465973da6d1a315a))
* **event-types:** 🐛 evict wrapped-write overlap behind older ring survivor ([e663ae9](https://github.com/muthmann/augur-rs/commit/e663ae993e0dd9020f2437c54e5eadb80f8e30d2))
* **gui,core:** 🐛 stop the UI claiming more than it can back up ([dd77d9b](https://github.com/muthmann/augur-rs/commit/dd77d9bb4fdcbd2bf65b8d25fa26e6485b930935))
* **gui:** 🐛 align replay 2D/3D windows through seek, step, and pause ([6fbedb7](https://github.com/muthmann/augur-rs/commit/6fbedb77b3951533f63be61624b80356c4a6e64c))
* **gui:** 🐛 keep plugin host views repainting without input events ([416115c](https://github.com/muthmann/augur-rs/commit/416115ccf6c50a6f7e20c5e45a4e5d48e3bd7423))
* **gui:** 🐛 keep the host-view dock inside its panel and closable ([0611fff](https://github.com/muthmann/augur-rs/commit/0611fff032db0db2100c111cc8514dd7f2b1b7e0))
* **gui:** 🐛 keep the sensor readout section visible and open ([18f6331](https://github.com/muthmann/augur-rs/commit/18f6331c8108deb4ddc53629e6d44879209ba165))
* **gui:** 🐛 persist the sensor-monitoring recording switch ([d43652a](https://github.com/muthmann/augur-rs/commit/d43652a42418f658b32c23e2aaabed1df6a27642))
* **gui:** 🐛 stop the dock tab strip deadlocking the UI thread ([45f8a04](https://github.com/muthmann/augur-rs/commit/45f8a04ac0758bf30300a9236f907181027d36e0))
* **gui:** add explicit f32 suffixes to float literals ([15dbf5a](https://github.com/muthmann/augur-rs/commit/15dbf5af9355b56d1ceb694b2f8c7b1f0abacdec))
* **gui:** fix panel layout bugs and visual polish ([f23c634](https://github.com/muthmann/augur-rs/commit/f23c634af1f09017535b57a7e5d0db7aa7a734c9))
* **prophesee:** 🐛 build the async USB reader on Windows ([92b0464](https://github.com/muthmann/augur-rs/commit/92b04648e35c9b3aa73a232f24ce97946eec26b4))
* **release:** 🐛 name the AppImage icon to match the desktop entry ([1dbef92](https://github.com/muthmann/augur-rs/commit/1dbef92a293ea3ce91cdbf9b43ba41863295ed13))
* **release:** 🐛 resolve the packaging output directory to an absolute path ([194e2bd](https://github.com/muthmann/augur-rs/commit/194e2bd26608b2183131b92eb4ba4b764679b2dd))
* **runtime:** 🐛 give plugin-runtime state explicit host/worker owners ([1984b4a](https://github.com/muthmann/augur-rs/commit/1984b4a6a4ba191f05ac6107a9b5b4440e9ddc53))
* **runtime:** 🐛 refresh plugin settings schema at the UI cache cadence ([21800ff](https://github.com/muthmann/augur-rs/commit/21800ff8aff2ce76167ecacb9316678ff40bb49e))


### Performance Improvements

* ⚡️ cut per-frame point-cloud re-decoding and dedupe hot paths ([f17a4d0](https://github.com/muthmann/augur-rs/commit/f17a4d0b9fbd99a4a739131118de6e5feba9e81e))
* **gui:** ⚡️ cut per-frame 3D scene and upload costs, fix translucent occlusion ([ea7f948](https://github.com/muthmann/augur-rs/commit/ea7f948b1ae7203787ec32a054040dce3a37c7fe))
* **prophesee:** ⚡️ queue async multi-URB bulk reads on the stream endpoint ([0141415](https://github.com/muthmann/augur-rs/commit/0141415c4c7dcd2d13ffe540f165c203ff0f2f35))
* **runtime:** ⚡️ decode host-view datasets on the worker, change-driven ([ae21e6d](https://github.com/muthmann/augur-rs/commit/ae21e6d058274f65d833de1f1d58ff2be9bd5f8b))

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
