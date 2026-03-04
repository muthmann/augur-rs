# ADR 001: Trait-Based Camera Abstraction

## Status
Accepted

## Context
AugurRS must support EVK4/IMX636 immediately and remain extensible to future Prophesee sensors without changing CLI/GUI logic.

## Decision
Use two core traits:
- `EventCamera`: lifecycle/config/info
- `PacketStreamCamera`: raw packet read capability for streaming

In `augur-prophesee`, use a dedicated `PseeSensor` trait for sensor-specific register programming.

## Consequences
- New sensors require only a new `PseeSensor` implementation and EVK4 detection wiring.
- Higher-level components remain backend-agnostic.
- Transport and sensor responsibilities are clearly separated.
