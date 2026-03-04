# Performance Notes

This project is performance-conscious, but it does not yet publish a formal benchmark against Prophesee's official software stack.

## What The Current Implementation Optimizes For

- bounded recording pipeline to avoid unbounded memory growth
- lossy preview path so UI rendering does not block capture
- typed configuration with direct runtime updates
- direct EVK4/Treuzell and IMX636 control path in Rust

These choices are aimed at keeping capture reliable and the code path understandable.

## What We Can Honestly Say Today

- The codebase is smaller and more focused than the full Metavision/OpenEB ecosystem.
- The recording and preview architecture is designed to behave predictably under load.
- The Rust implementation avoids additional language bindings and large framework layers in the core capture path.

Those are architectural advantages. They are not the same thing as a published end-to-end performance win.

## What We Cannot Claim Yet

Until the same workload is measured side by side, this repository should not claim:

- lower latency than Metavision/OpenEB
- higher sustained event throughput than Metavision/OpenEB
- lower CPU or memory usage than the official stack

## Recommended Benchmark Plan

Benchmark on the same:

- EVK4 + IMX636 hardware
- host machine
- cable path
- scene
- recording target

Measure at least:

- sustained event rate during recording
- dropped preview frames or preview freshness
- CPU usage
- memory usage
- startup time
- total bytes written for fixed-duration captures

## USB Matters

Connection quality can materially affect observed throughput and stability.

In particular:

- prefer direct USB 3 connections
- avoid questionable hubs and adapters when possible
- keep cable changes in your benchmark notes

If a direct USB-C cable performs better than a USB-A path with an adapter, treat that as a real experimental variable, not just a curiosity.
