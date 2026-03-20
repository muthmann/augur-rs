# Performance Safeguards

## Why This Exists

The live GUI and replay preview are intentionally allowed to be lossy so the recording path can stay bounded and predictable. This note captures the runtime safeguards that were added after severe lag reports in live and replay mode.

## Recording-Safety Changes

- The output file and EVT3 header are prepared before the camera starts streaming. If file creation or header writing fails, the pipeline now fails fast instead of starting capture and erroring asynchronously later.
- Once the USB reader has accepted a packet, shutdown now keeps trying to enqueue that packet to the disk writer instead of discarding it when stop and backpressure overlap.
- The USB reader no longer scans every packet to estimate event counts for GUI/CLI stats. Ingress bytes remain authoritative on the capture path, while decoded-event stats are now measured on the lossy preview side.

## Preview / GUI Changes

- Preview packet fan-out now uses a reusable buffer pool instead of allocating a fresh `Vec<u8>` for every packet on the USB thread.
- Preview packet cloning is now lazy: the USB reader only copies bytes into the preview path when a preview buffer and queue slot are actually available.
- Preview frames now recycle their large `pixels`, `pixels_on`, and `pixels_off` buffers instead of cloning full frame images on every acquisition window.
- `PipelineStatsSnapshot` now exposes preview packet/frame drops, preview/disk queue high-water marks, and cumulative disk send/write time so overload is visible in the GUI.
- The GUI only processes preview work at a capped cadence of roughly 30 Hz. This keeps egui from spending all of its time on texture uploads, overlay generation, and plugin work when frames arrive faster than the UI can present them.
- Point-cloud mode now runs at a lower presentation cadence than the 2D preview path so 3D view does not force the same repaint rate as texture mode.
- Preview rendering now writes directly into `ColorImage` pixels as `Color32`, with reused histogram and ROI-grid scratch storage, instead of rebuilding an intermediate RGBA buffer every frame.
- Paused replay no longer forces a continuous repaint loop.
- Decoded replay opening now runs on a worker thread, so large decoded replays do not block the egui thread before playback begins.
- When 3D point-cloud view is the only raw-event consumer, the GUI skips runtime-plugin FFI event marshaling and `EventStore` updates.
- Dynamic-plugin settings/status reads are cached for 250 ms and invalidated on runtime mutations instead of being re-polled every update.
- Host-view dataset snapshots are now cached by dataset generation instead of being cleared unconditionally on every processed frame.

## Operational Implications

- Recording correctness takes priority over preview smoothness and preview completeness.
- `MB/s` remains the best throughput indicator for capture health because it is measured directly on the ingress path.
- `Mev/s` now reflects decoded preview traffic. Under extreme preview backpressure it can under-report relative to the true ingress event rate, which is an intentional tradeoff to keep the USB reader lightweight.
- Camera/USB overflow telemetry still depends on lower-level transport support. The current host telemetry covers queue pressure and write stalls, but it does not invent device-side overflow counters that the transport does not expose yet.
