# Performance Safeguards

## Why This Exists

The live GUI and replay preview are intentionally lossy so the recording path can stay bounded and predictable. This note captures the runtime safeguards that keep live and replay mode responsive under load.

## Recording-Safety Changes

- The output file and EVT3 header are prepared before the camera starts streaming. If file creation or header writing fails, the pipeline now fails fast instead of starting capture and erroring asynchronously later.
- Once the USB reader has accepted a packet, shutdown now keeps trying to enqueue that packet to the disk writer instead of discarding it when stop and backpressure overlap.
- The disk writer thread drains its queue until the USB reader hangs up the channel, never on the stop flag. Exiting on the stop flag raced the USB reader's final packet (a bulk read can complete just after stop is requested) and silently dropped the tail of the recording. A regression test (`disk_writer_persists_packet_accepted_after_stop_request`) pins this.
- The in-flight budget between the USB reader and the disk writer (`RAW_BUFFER_POOL_CAPACITY` / `DISK_QUEUE_CAPACITY`) is 256 × 64 KiB = 16 MiB. This budget is what rides out disk-write hiccups: once the pool is empty the USB reader stops reading and the camera-side FIFO overflows, which shows up as a hard multi-millisecond gap in the recording. The previous 8-buffer (512 KiB) budget only covered ~2.5 ms at EVK4 peak ingress — less than a single 4 MiB `BufWriter` flush on a slow disk. The path stays bounded and explicit; only the bound grew.
- The USB reader does not scan every packet to estimate event counts. Ingress bytes remain authoritative on the capture path, while decoded-event stats are measured on the lossy preview side.
- Camera control and stream reads now run on separate threads for split-capable cameras (EVK4): sensor reconfiguration (USB control transfers) executes on a dedicated control thread while the stream thread keeps reading, so applying settings during a recording no longer pauses reads or overflows the device FIFO (see ADR 023). A regression test (`split_camera_keeps_reading_during_runtime_reconfigure`) pins this.
- The GUI asks for confirmation before applying settings during an active recording, because the reconfigure itself (bias/ROI/filter changes and a brief sensor disturbance) is still part of the recorded data even though the stream stays gap-free.
- EVK4 stream reads use an asynchronous multi-URB reader (8 × 64 KiB bulk transfers queued in the kernel at all times), so the host keeps receiving data between `read_packet` calls with no dead time. If async setup fails, the reader falls back to synchronous single-transfer reads and logs the reason.

## Preview / GUI Changes

- Preview packet fan-out now uses a reusable buffer pool instead of allocating a fresh `Vec<u8>` for every packet on the USB thread.
- Preview packet cloning is now lazy: the USB reader only copies bytes into the preview path when a preview buffer and queue slot are actually available.
- Preview frames now recycle their large `pixels`, `pixels_on`, and `pixels_off` buffers instead of cloning full frame images on every acquisition window.
- `PipelineStatsSnapshot` now exposes preview packet/frame drops, preview/disk queue high-water marks, and cumulative disk send/write time so overload is visible in the GUI.
- The GUI only processes preview work at a capped cadence of roughly 30 Hz. This keeps egui from spending all of its time on texture uploads, overlay generation, and live-analysis dispatch when frames arrive faster than the UI can present them.
- Point-cloud mode now runs at a lower presentation cadence than the 2D preview path so 3D view does not force the same repaint rate as texture mode.
- Preview rendering now writes directly into `ColorImage` pixels as `Color32`, with reused histogram and ROI-grid scratch storage, instead of rebuilding an intermediate RGBA buffer every frame.
- Paused replay no longer forces a continuous repaint loop.
- Decoded replay opening now runs on a worker thread, so large decoded replays do not block the egui thread before playback begins.
- Live runtime plugins run on a dedicated `augur-runtime` analysis worker. The
  worker may coalesce frame triggers, but retained-history plugins drain all
  upstream frames through their own cursor before processing.
- When 3D point-cloud view is the only raw-event consumer, the GUI skips runtime-plugin FFI event marshaling and `EventStore` updates.
- When no raw-event consumer is active, the preview thread skips per-event raw
  retention and live-ring archival entirely; count-plane preview accumulation
  still runs normally.
- Enabled `FrameOnly` / `DerivedData` plugins no longer force current-frame
  `FfiCdEvent` materialization. Only enabled `RawEvents` plugins receive the
  raw slice for the current frame.
- Dynamic-plugin settings/status reads are cached for 250 ms and invalidated on runtime mutations instead of being re-polled every update.
- Plugin `EventStoreHandle::frame_at` calls reuse a per-plugin-call
  materialization cache, and global settings context JSON is cached until the
  effective settings change.
- Host-view dataset snapshots are now cached by dataset generation instead of being cleared unconditionally on every processed frame.

## Operational Implications

- Recording correctness takes priority over preview smoothness and preview completeness.
- `MB/s` remains the best throughput indicator for capture health because it is measured directly on the ingress path.
- `Mev/s` now reflects decoded preview traffic. Under extreme preview backpressure it can under-report relative to the true ingress event rate, which is an intentional tradeoff to keep the USB reader lightweight.
- Camera/USB overflow telemetry still depends on lower-level transport support. The current host telemetry covers queue pressure and write stalls, but it does not invent device-side overflow counters that the transport does not expose yet.
