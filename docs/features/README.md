# Technical Notes

These notes capture feature-level behavior and implementation constraints useful for maintainers and advanced users.

## Plugin Ecosystem

This repository defines the plugin API, runtime loader, host-side analysis UI, and built-in
tools (hotpixel detection, ROI-grid overlay). The plugin template, authoring docs, and
community plugin implementations live in the companion
**[augur-plugins](https://github.com/muthmann/augur-plugins)** repository.

See the [Plugin Authoring Guide](./plugin-authoring-guide.md) for the interface spec and [Dynamic Plugin Loading](./dynamic-plugins.md) for the install/reload workflow.

---

## Index

- [AugurRS SDK Overview](./augur-sdk.md) — shared backend, runtime guarantees, IMX636-specific behavior, recording details
- [Performance Safeguards](./performance-safeguards.md) — capture-path fail-fast setup, stop-time drain behavior, preview throttling, and UI-side load shedding
- [Performance Overhaul](./performance-overhaul.md) — split pipeline telemetry, direct `Color32` preview rendering, async decoded replay opening, and generation-aware host caching
- [WGPU Preview Rendering](./wgpu-preview-rendering.md) — dual-backend startup, shader-driven preview textures, shared overlay painting, and preview perf telemetry
- [Global Settings Menu and Replay Pacing](./global-settings-menu.md) — top-bar settings controls, hover/reference guidance, persisted `[global]` config, plugin `GlobalSettings`, replay-editable acquisition time, and speed-change replay resets
- [Replay Timing Model](./replay-timing-model.md) — timestamp-driven replay pacing, auto-derived replay display cadence, and preview-frame load shedding
- [TIFF Stack Export](./tiff-stack-export.md) — File-menu batch export of replay accumulations to multi-page 16-bit TIFF with time-window and ROI controls
- [Release Distribution](./release-distribution.md) — cross-platform tagged artifacts, macOS signing/notarization, and cargo-release workflow
- [Local Desktop Install](./local-desktop-install.md) — one-command macOS source install plus current release expectations on Linux and Windows
- [HDF5 File Support](./hdf5-file-support.md) — native HDF5 build requirements, ECF plugin installation, runtime environment setup
- [Rich Recording Metadata](./rich-recording-metadata.md) — self-describing EVT3 headers, metadata sidecars, and replay provenance
- [Replay](./raw-replay.md) — `.raw` plus decoded-event replay, seekable transport controls, persisted EOF state
- [Python Event Ingress](./python-event-ingress.md) — loopback `evt3.augur` connector support for publishing NumPy event arrays into the existing Augur preview pipeline
- [Theme-Aware GUI Status Colors](./theme-aware-gui-status-colors.md) — theme-adaptive warning/error labels plus contrast-safe success/info colors
- [Design System](./design-system.md) — workbench palette, spacing/radius tokens, magenta/cyan polarity preview, condensed status footer, and the `theme::visuals` builder used by every panel
- [Collapsible GUI Panels](./collapsible-panels.md) — separator-edge collapse arrows plus scrollable settings and analysis panels
- [Interactive Preview Workbench](./interactive-preview-workbench.md) — shared 2D/3D preview state, idle launch shortcuts, ROI drag tool, zoom/crop, popup enlarge, point cloud
- [Investigation Workspace](./investigation-workspace.md) — host-owned 2D/3D layouts, linked selection, stable row identities, generic layer styling, and popup/shared inspection state
- [Investigation Dataflow And Memory Model](./investigation-dataflow-and-memory-model.md) — end-to-end ownership, allocation, upstream raw-event source target, 2D/3D render data flow, replay seeking interactions, and plugin boundaries
- [Investigation Thread And Execution Model](./investigation-thread-and-execution-model.md) — OS thread inventory, channel/atomic map, where calculations vs rendering vs plugins actually run, and hot-path bottlenecks
- [Analysis Execution Model](./analysis-execution-model.md) — `augur-runtime`, live analysis worker, deterministic offline pipeline, ABI v4 lifecycle, and raw-event hot-path gates
- [Investigation Table Trustworthiness](./investigation-table-trustworthiness.md) — declarative TableV1 provenance/display metadata, summary card, paginated TableWindow, auto-seek on row selection
- [Clickable 2D Overlays And Failed-Fit Inspection](./investigation-clickable-overlays.md) — marker `source_row`, rejected-fit diamond selection, reason-first inspector, cross-span persistence
- [Investigation Action Requests](./investigation-action-requests.md) — plugin-declared dataset/row/cluster actions, host-rendered modal, request bus, re-fit/commit/discard flow
- [Viewer Tools And ImageJ Bridge](./viewer-tools-and-imagej.md) — host-side histogram/colormap/measurement tools plus the bundled Augur Bridge handoff path for ImageJ/Fiji
- [Reusable Viewer Widget](./reusable-viewer-widget.md) — one shared central viewer component for the main window and popup host
- [Viewer Toolbar And Status Layout](./viewer-toolbar-and-status-layout.md) — consistent toolbar/display-strip/footer structure across the 2D and 3D viewers
- [Built-In Hotpixel Detection](./built-in-hotpixel-detection.md) — host-owned hotpixel detection, overlays, and DEM-mask copy flow
- [Plugin Authoring Guide](./plugin-authoring-guide.md) — runtime-only plugin host, FFI API, phased execution, and shared context types
- [Host View Registry](./host-view-registry.md) — generic dataset/view descriptors, precedence rules, host-rendered tables, plots, and images
- [Dynamic Plugin Loading](./dynamic-plugins.md) — plugin directory layout, manifest expectations, and Plugin Manager workflow
- [Output File Timestamps](./output-file-timestamps.md) — overwrite protection and automatic timestamp suffixes for recordings

Community runtime plugins, the plugin template, and contributor docs are maintained in the companion [augur-plugins](https://github.com/muthmann/augur-plugins) repository.
