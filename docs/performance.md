# Performance

AugurRS is designed around a small set of architectural choices that keep capture reliable and latency predictable.

## Pipeline Architecture

The recording pipeline uses three dedicated threads:

1. **USB reader** — pulls raw EVT3 packets from the EVK4 as fast as the device delivers them
2. **Disk writer** — receives packets through a bounded channel with backpressure. If the disk can't keep up, the channel blocks the reader rather than growing memory indefinitely
3. **Preview decoder** — receives packets through a lossy channel (`try_send`). Frames are dropped rather than blocking the USB hot path

This means capture throughput is never throttled by UI rendering, and memory usage stays bounded regardless of event rate or recording duration.

## Direct Control Path

AugurRS talks to the EVK4 through its own Treuzell USB transport and IMX636 register interface — no C++ bindings, no vendor runtime, no plugin system in the capture path. The result is a short, auditable code path from sensor to disk.

## Runtime Updates Without Restart

Biases, ROI, pixel mask, digital filters, and acquisition window can all be changed mid-session. Updates are applied through the same register interface without stopping the pipeline or restarting the USB stream.

## What This Means in Practice

- Recording sessions don't accumulate memory over time
- Preview lag doesn't affect capture integrity
- The full capture path fits in a single workspace you can read end-to-end
- Startup is fast — no SDK initialization, no plugin discovery

## Benchmarking

Formal side-by-side benchmarks against Metavision/OpenEB have not been published yet. The architectural properties above (bounded memory, non-blocking capture, direct USB path) are structural — they hold by construction, not by tuning.

If you run your own comparisons, control for: hardware model, host machine, USB path (direct USB 3 — avoid hubs), scene, and recording target.
