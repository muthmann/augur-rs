# Architecture Decision Records

ADRs capture lasting technical decisions, the context behind them, and their consequences.

## When to Add an ADR

Add an ADR when a change:

- establishes or replaces an architectural pattern
- commits the project to a long-lived technical direction
- introduces a tradeoff future contributors need to understand

Name files as `docs/adr/<nnn>-<slug>.md` and keep this index updated.

## Index

- [ADR 001: Trait-Based Camera Abstraction](./001-trait-based-camera-abstraction.md)
- [ADR 002: Three-Thread Streaming Pipeline](./002-streaming-pipeline-design.md)
- [ADR 003: Dynamic Plugin Loading via C FFI and `libloading`](./003-dynamic-plugin-loading.md)
- [ADR 004: Cross-Platform Release Pipeline With macOS DMG Distribution](./004-cross-platform-release-pipeline.md)
- [ADR 005: Shared Preview Workspace State for 2D/3D GUI Views](./005-shared-preview-workspace.md)
- [ADR 006: Host-Owned Dataset/View Registry for Plugin Outputs](./006-host-view-registry.md)
- [ADR 007: Host-Owned EventStore for Plugin History](./007-event-store-history.md)
- [ADR 008: Segmented EventStore and Generation-Aware Host View Caching](./008-segmented-event-store-and-host-view-generations.md)
- [ADR 009: Host-Owned Global Settings Contract](./009-host-global-settings-contract.md)
- [ADR 010: Host-Owned Viewer Tools And External Preview Bridges](./010-host-owned-viewer-tools-and-external-bridges.md)
- [ADR 011: Reusable Viewer Widget For Embedded And Popup Hosts](./011-reusable-viewer-widget.md)
- [ADR 012: Generic Plugin Boundary With Companion Domain-Type Crates](./012-generic-plugin-boundary-and-companion-types.md)
- [ADR 013: Self-Describing Recording Metadata](./013-self-describing-recording-metadata.md)
- [ADR 014: Timestamp-Driven Replay Pacing](./014-timestamp-driven-replay-pacing.md)
- [ADR 015: Dual-Backend Preview Rendering With WGPU Presentation](./015-dual-backend-preview-rendering.md)
- [ADR 016: Host-Owned Generic Investigation Workspace](./016-generic-investigation-workspace.md)
- [ADR 017: Declarative TableV1 Provenance And Display Metadata](./017-declarative-tablev1-metadata.md)
- [ADR 018: Host Action Bus For Plugin-Declared Actions](./018-host-action-bus.md)
- [ADR 020: Unified Upstream Event Source](./020-unified-upstream-event-source.md)
- [ADR 021: Python Event Ingress Through Packed Preview Pipeline](./021-python-event-ingress.md)
- [ADR 022: Analysis Runtime, Live Worker, And Offline Pipeline](./022-analysis-runtime-live-worker-and-offline-pipeline.md)
- [ADR 023: Split Control And Stream Transport Threads](./023-split-control-and-stream-transport.md)
- [ADR 024: Host/Worker Plugin State Ownership](./024-host-worker-plugin-state-ownership.md)
- [ADR 025: Analysis Runs As The Primary Analysis Interface](./025-analysis-runs-primary-interface.md)
- [ADR 026: External-Trigger Delivery And Plugin Execution Context (ABI v5)](./026-external-triggers-and-execution-context.md)
- [ADR 027: Worker-Owned Semantic Plugin Services](./027-plugin-setting-requests.md)
- [ADR 028: Allowlisted Plugin to Host Recording Commands](./028-plugin-host-command-queue.md)
- [ADR 029: Sensor Monitoring Readback For Absolute Setting Values](./029-sensor-monitoring-readback.md)
- [ADR 030: Reader-Owned Transfer Buffers And Recording Completeness Accounting](./030-recording-completeness-accounting.md)
- [ADR 031: Optional Sensor-Telemetry Companion Recording](./031-sensor-telemetry-companion-recording.md)
