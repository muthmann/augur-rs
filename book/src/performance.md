# Performance Notes

This project is performance-conscious, but it does not yet publish a formal benchmark against Prophesee's official software stack.

## What The Current Implementation Optimizes For

- bounded recording pipeline to avoid unbounded memory growth
- lossy preview path so UI rendering does not block capture
- typed configuration with direct runtime updates
- direct EVK4/Treuzell and IMX636 control path in Rust

## What We Can Honestly Say Today

- The codebase is smaller and more focused than the full Metavision/OpenEB ecosystem.
- The recording and preview architecture is designed to behave predictably under load.
- The Rust implementation avoids additional language bindings and large framework layers in the core capture path.

## What We Cannot Claim Yet

Until the same workload is measured side by side, this repository should not claim:

- lower latency than Metavision/OpenEB
- higher sustained event throughput than Metavision/OpenEB
- lower CPU or memory usage than the official stack

## Recommended Benchmark Plan

Measure on the same:

- EVK4 + IMX636 hardware
- host machine
- cable path
- scene
- recording target

Capture at least:

- sustained event rate during recording
- dropped preview frames or preview freshness
- CPU usage
- memory usage
- startup time
- total bytes written for fixed-duration captures
