# ADR 001: Trait-Based Camera Abstraction

## Status
Accepted

## Context
AugurRS must support event camera sensors and remain extensible to future hardware without changing CLI/GUI logic.

## Decision
Use two core traits:
- `EventCamera`: lifecycle/config/info
- `PacketStreamCamera`: raw packet read capability for streaming

In `augur-prophesee`, use a dedicated `PseeSensor` trait for sensor-specific register programming.

## Consequences
- New sensors require only a new `PseeSensor` implementation and detection wiring.
- Higher-level components remain backend-agnostic.
- Transport and sensor responsibilities are clearly separated.
