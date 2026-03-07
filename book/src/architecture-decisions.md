# Architecture Decisions

AugurRS keeps long-lived design choices in ADRs under `docs/adr/`.

## Current Decisions

- **ADR 001: Trait-Based Camera Abstraction**: `augur-core` defines `EventCamera` and `PacketStreamCamera` so the CLI and GUI stay backend-agnostic while current hardware support remains EVK4 / IMX636 focused.
- **ADR 002: Three-Thread Streaming Pipeline**: the USB reader, disk writer, and preview worker are split so recording stays bounded and preview can be intentionally lossy under load.
- **ADR 003: Dynamic Plugin Loading via C FFI and `libloading`**: runtime-loaded analysis plugins now cross an explicit ABI boundary so plugin crates can be rebuilt and reloaded without recompiling `augur-gui`.

When a future change establishes a lasting architectural pattern or commits the project to a non-obvious tradeoff, add a new ADR and update the index in `docs/adr/README.md`.
