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
