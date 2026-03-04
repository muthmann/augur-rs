# ADR 002: Three-Thread Streaming Pipeline

## Status
Accepted

## Context
The camera stream must feed disk persistence and live preview simultaneously with predictable behavior under load.

## Decision
Adopt a three-thread model:
1. USB thread owns camera and reads packets from bulk endpoint.
2. Disk thread writes `.raw` using bounded queue to enforce backpressure.
3. Preview thread decodes/accumulates packets and emits frames over lossy channel.

Shared acquisition time (`acq_time_us`) is an `Arc<AtomicU64>` updated by GUI.
Worker failures are published over an internal error channel consumed by CLI/GUI.
USB stream read timeouts are treated as non-fatal to support sparse scenes.

## Consequences
- Disk integrity is prioritized: USB pauses if disk is slow.
- Preview remains responsive by dropping stale packets rather than blocking hot path.
- Runtime setting updates can be applied without restarting the pipeline.
- Fatal worker failures are no longer silent and stop the pipeline deterministically.
