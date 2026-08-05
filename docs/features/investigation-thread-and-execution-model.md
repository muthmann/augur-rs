# Investigation Thread And Execution Model

## Summary

This note is the threading-side companion to
[`investigation-dataflow-and-memory-model.md`](./investigation-dataflow-and-memory-model.md).
The dataflow note explains *what* data exists and *who owns it*. This note
explains *which OS thread* each piece of work actually runs on, and where the
hot paths are.

The short version is:

- the **camera/replay pipeline** owns 2–3 background OS threads spawned by
  `augur_core::pipeline::spawn_pipeline`: a USB/file reader, a preview
  decoder/accumulator, and (only during recording) a disk writer
- the **GUI** is otherwise single-threaded: the egui/eframe `update()` callback
  drains preview frames, runs hotpixel + plugins, builds 2D/3D scenes,
  encodes WGPU command buffers, and submits them — all on the same thread that
  also handles input
- **WGPU** does not give us a separate render thread. Compute and render passes
  are encoded synchronously on the GUI thread; only the GPU work itself runs
  out-of-band on the driver
- **plugins** are dynamically loaded `cdylib`s and run **inline on the GUI
  thread** through the `PluginVTable` FFI. They cannot offload work to a host
  thread pool today; a slow plugin freezes the UI
- a few **one-shot worker threads** are spawned for blocking work that must
  not stall the GUI: opening a decoded replay, writing a TIFF stack, and the
  ImageJ TCP bridge

So the realistic picture is: three steady-state pipeline threads, one busy GUI
thread that doubles as the renderer and plugin host, and a small handful of
short-lived helper threads. There is no Tokio runtime and no Rayon thread pool
in this codebase — concurrency is hand-rolled with `std::thread` plus
`crossbeam_channel` and a few `Arc<Mutex<…>>` shared structures.

## Thread Inventory

| Thread | Spawned in | Lifetime | Purpose |
|---|---|---|---|
| **GUI / main** | `eframe::run_native` (`render_backend.rs:83`) | process | egui repaints, all UI, plugin host, 2D/3D scene assembly and submission |
| **`usb_thread`** | `pipeline.rs:1029` | until pipeline stop | reads packets from camera or replay file, fans out to preview + disk channels |
| **`disk_thread`** | `pipeline.rs:1205` | recording session | flushes raw EVT3 packets to disk |
| **`preview_thread`** | `pipeline.rs:1257` | until pipeline stop | decodes EVT3 → `CdEvent`, accumulates count planes / histograms, appends to live event ring, emits `PreviewFrame` |
| **Replay open task** | `app.rs:3889` | one-shot | parses decoded replay (`.csv` / `.bin` / `.npy` / `.h5`) into `Arc<Vec<CdEvent>>` without blocking egui |
| **TIFF export task** | `app.rs:4081` | one-shot | writes a TIFF stack from a stored event source |
| **ImageJ bridge worker** | `external_tools/imagej.rs:87` | until disconnect | TCP `connect` + write loop streaming `FrameEnvelope`s to ImageJ |
| **WGPU device polling** | implicit in `wgpu`/`egui-wgpu` | runtime-managed | not owned by this codebase; driver-side work for GPU submissions |

There is **no** dedicated audio thread, no Tokio executor, no Rayon pool, no
plugin-side thread, and no separate render thread.

## Channels And Shared State

### Crossbeam channels (steady-state pipeline)

All pipeline channels are bounded `crossbeam_channel` queues. Capacities are
small and tuned for backpressure rather than buffering.

| Channel | From → To | Capacity | Item |
|---|---|---|---|
| raw buffer pool | GUI / preview / disk → USB | `RAW_BUFFER_POOL_CAPACITY = 8` | empty `Vec<u8>` for camera reads |
| preview packet pool | preview → USB | `PREVIEW_PACKET_POOL_CAPACITY = 4` | empty buffer for preview-side copy |
| `preview_tx` | USB → preview | `PREVIEW_PACKET_QUEUE_CAPACITY = 4` | `PreviewChunk` (raw EVT3 packet copy) |
| `disk_tx` | USB → disk | `DISK_QUEUE_CAPACITY = 8` | `DiskChunk` (raw EVT3 packet) |
| `frame_tx` | preview → GUI | `PREVIEW_FRAME_QUEUE_CAPACITY = 4` | `PreviewFrame` |
| `settings_tx` | GUI → USB | bounded | `CameraConfig` reconfigure |
| `error_tx` | any pipeline → GUI | bounded | error string |

Two facts matter for hot-path reasoning:

- the `frame_tx` / `preview_tx` queues are tiny (4). Once they fill, the USB
  thread drops preview packets (recording the drop in stats) and lets the disk
  path absorb backpressure with a `send_timeout(50ms)` then a blocking `send`.
  Disk is therefore allowed to push back on USB; preview is not.
- the GUI drains `frame_rx` exhaustively each `update()` tick
  (`app.rs:4976`), so the queue is normally empty when the GUI is idle.

### One-shot mpsc channels

- `app.rs:3887` — replay-open result (`mpsc::channel`)
- `app.rs:4079` — TIFF export result (`mpsc::channel`)
- `preview_renderer.rs:2629` — synchronous WGPU buffer-readback callback used
  inside `device.poll(Maintain::Wait)` (a single-shot `std::sync::mpsc`)

### Shared mutable state

- `Arc<Mutex<EventRing>>` inside `LiveEventSource` (`pipeline.rs:71`):
  the upstream raw-event ring. Locked by the preview thread on every frame
  emission (append) and by the GUI thread when materializing events for the
  3D cloud, plugin retained history, or replay snapshots.
- `Arc<Mutex<PipelineStatsInner>>` (`pipeline.rs:998`): stats are written by
  USB / disk / preview threads and read by the GUI status panel.
- `OnceLock<Mutex<Vec<PreviewFrameBuffers>>>` (`pipeline.rs:643`): the
  global `PreviewFrame` buffer pool. Locked when the preview thread acquires a
  buffer and when a `PreviewFrame::drop` returns one.
- `Arc<Mutex<ExternalToolStatus>>` (`external_tools/imagej.rs:47`): touched
  by GUI and the bridge worker.
- Atomics on `PipelineController` (`pipeline.rs:848`):
  `stop: AtomicBool`, `raw_events_needed: AtomicBool`, `acq_time_us: AtomicU64`
  — set by GUI, read by USB/preview without locking.
- Atomics on `ReplayControls` (`replay.rs:62`): `paused`, `speed_bits`,
  `speed_epoch`, `bytes_read`, `current_timestamp_us` — written by USB during
  replay, read by both GUI and the throttle loop.
- `thread_local! PREVIEW_SCRATCH` (`preview.rs:8`): time-surface staging
  vectors local to the GUI thread (the only thread that calls
  `with_prepared_preview_frame`).

There is **no** `RwLock`, no `parking_lot`, and no async primitives.

## Where Work Actually Runs

### USB / capture thread (`usb_thread`)

Started by `spawn_pipeline` and continuously:

1. drains `settings_rx` and reconfigures the camera if needed;
2. waits up to 10 ms for an empty raw buffer from the pool;
3. calls `camera.read_packet(buf)` (live USB or replay-file `read`);
4. records packet stats;
5. tries to copy the packet into a preview-side buffer and `try_send`s it
   onto `preview_tx`. If `preview_tx` is full, the packet is dropped for
   preview and a stats counter is incremented;
6. forwards the original buffer to `disk_tx` with `send_timeout(50 ms)` —
   then a blocking `send` if the timeout hits — so disk pressure can
   actually stall capture if the SSD cannot keep up.

For `RawFileCamera`, `read_packet` itself sleeps to throttle replay to wall
clock (`replay.rs:349`) and inspects the `paused` atomic each iteration
(`replay.rs:412`). For `DecodedEventFileCamera`, the same throttling logic
runs at `decoded_replay.rs:268`. So replay timing happens **on this thread**,
not in the GUI.

### Disk writer thread (`disk_thread`)

Only started when `disk_writer` is `Some` (recording). It does nothing but
`recv_timeout(20 ms)` from `disk_rx`, write all bytes to a `BufWriter<File>`,
return the buffer to the pool, and record write-time stats. On stop it
flushes once. There is no compression and no fsync.

### Preview / decode thread (`preview_thread`)

This is the second hot path after the GUI. Per-iteration cost:

1. `recv_timeout(2 ms)` from `preview_rx`;
2. `decoder.decode_bytes(...)` → reusable `Vec<CdEvent>`;
3. for every event, update `pixels`, `pixels_on`, `pixels_off`, transition the
   total and signed histograms, and optionally push the event into a per-frame
   `Vec<CdEvent>` (only when `raw_events_needed`);
4. when the acquisition window closes, `emit_preview_frame` appends the closed
   frame into the live `EventRing` only when `raw_events_needed`, advances
   recording / plugin cursors for that archived range, and `try_send`s the
   `PreviewFrame` to `frame_tx`.

This loop is the **per-event** code path. Anything that touches per-event
state during accumulation (extra histogram bins, conditional copies) shows up
proportional to event rate. The optional `frame_events.push(*ev)` inside the
hot accumulation loop is metered separately as
`record_preview_raw_event_copy_time` — that flag should stay off when no
consumer needs raw events.

### GUI / main thread (`eframe::App::update`)

Each repaint, in order (`app.rs:5210`):

1. `apply_theme_to_ctx`
2. `poll_replay_open_task`
3. `poll_tiff_stack_export_task`
4. **`poll_live_analysis_results()`** — drains epoch-tagged worker results and
   publishes only current live plugin overlays/host-view snapshots.
5. **`update_preview_texture(ctx)`** — this is where most steady-state work
   lives:
   - exhaustively drain `frame_rx`. For every drained frame, push its
     upstream source/range projection into `PointCloudState`. Keep only the
     newest frame for further processing (`app.rs:4976–4979`).
   - for live preview, recording, and unpaused replay playback, send the newest frame to
     `LiveAnalysisWorker` instead of executing runtime plugins on this thread;
   - for paused replay scrubs and explicit recomputation, run the synchronous
     analysis path.
6. `poll_pipeline_state` (drains `error_rx`, refreshes stats / status)
7. `refresh_host_view_registry_if_dirty` — resolves plugin host-view descriptors
   from worker snapshots in live mode or the GUI plugin mirror otherwise
8. all egui panels: toolbar, settings, viewer widget (which may invoke the
   2D preview and 3D scene paths), histograms, etc.

Everything in steps 1–8 except the live worker itself runs on this single
thread. The **2D preview**
(`preview_renderer.rs`) and **3D investigation scene** (`inspection_3d.rs`)
both encode their command buffers here and submit through
`render_state.queue.submit(...)`.

### WGPU work

Both renderers use `egui_wgpu::RenderState` (cloned from
`eframe::CreationContext`) and own persistent GPU resources:

- `WgpuPreviewRenderer` (`preview_renderer.rs:825`) keeps multiple
  `wgpu::ComputePipeline`s for count + time-surface accumulation and
  histogram reduction, plus a render pipeline for the LUT-applied display
  texture.
- `WgpuInvestigation3dRenderer` (`inspection_3d.rs:426`) keeps a render
  pipeline, uniform buffer, depth/display attachments, and a grow-on-demand
  instance buffer.

For each GUI tick that produces a new render:

1. CPU-side scratch is built on the GUI thread (`prepare_scene_points`, time-
   surface extraction);
2. data is uploaded with `queue.write_buffer` / `queue.write_texture`;
3. a `CommandEncoder` records compute and render passes;
4. `queue.submit(...)` returns immediately; the GPU work runs out-of-band.

The GPU is therefore async, but the **CPU staging step is not**. A large
event count means a large `PointInstanceRaw` vector built on the GUI thread,
followed by a buffer upload that competes for the same thread budget as
plugin execution.

Two readback paths *do* synchronously block the GUI thread:

- `map_buffer_sync` (`preview_renderer.rs:2622`) calls
  `device.poll(Maintain::Wait)` and blocks until the GPU finishes a histogram
  or hover readback. This is the only place the GUI thread waits on the GPU,
  and it only fires for the WGPU preview path that needs CPU-visible histogram
  data.
- The 3D renderer registers an offscreen texture via
  `register_native_texture` (`inspection_3d.rs:622`) and submits without a
  readback; the result is consumed as an `egui::TextureId` next paint.

### One-shot helper threads

- **Replay open** (`app.rs:3889`): only spawned for decoded replay
  (`.csv` / `.bin` / `.npy` / `.h5`). Decoding the entire file into
  `Arc<Vec<CdEvent>>` can take seconds; running it on the GUI thread would
  freeze the window. Result is polled non-blocking via
  `poll_replay_open_task`. Raw `.raw` replay is light enough that it opens
  inline.
- **TIFF stack export** (`app.rs:4081`): same pattern.
- **ImageJ bridge** (`external_tools/imagej.rs:87`): owns the TCP socket and
  drains a bounded `crossbeam_channel`. The GUI thread calls
  `try_send(FrameEnvelope { … pixels: frame.pixels.clone() … })`, which is
  non-blocking — the only cost on the GUI side is the pixel-plane clone.

## Plugin Execution Model

This is the single most important threading fact about plugins today:

> **Live runtime plugins execute on the `augur-runtime` live analysis worker.
> Replay-time recomputation can still use `CameraApp::run_analysis`
> synchronously.**

Concretely, the worker owns its own `PluginManager`, `PluginEventHistory`, and
event-source cursor. The GUI keeps a separate plugin manager for settings,
status, and plugin-manager UI, then mirrors settings to the worker with
epoch-tagged configuration messages.

```text
for phase in [FrameOnly, RawEvents, DerivedData]:
    for record in plugin_manager.records_mut():
        if plugin.enabled() && plugin.input_kind() == phase:
            plugin.process_frame(...)   // FFI call into the plugin .so
```

Each worker-side `plugin.process_frame` is a
`(self.vtable.process_frame)(self.instance, …)` indirect call in
`augur-runtime` into a `cdylib` loaded with `libloading`. The plugin's view of
the world is the four FFI inputs from the plugin authoring guide:

- `FfiPreviewFrame` — borrows `frame.pixels` and the just-built
  `Vec<FfiCdEvent>`;
- `FfiOutputCallbacks` — overlays / markers / warnings, all of which run host
  code (e.g. `add_marker_overlay`) on the same thread that called the plugin;
- `FfiPluginContext` — `publish` / `get` / `publish_persistent` /
  `get_persistent` callbacks into the GUI's two `HashMap`s;
- `FfiEventStoreHandle` — `frame_count` / `frame_at` /
  `frame_range_for_timestamps` / `oldest_timestamp_us`. `frame_at` lazily
  materializes a frame slice from `PluginEventHistory` and keeps it alive in
  a per-call `RefCell<Vec<Box<[FfiCdEvent]>>>`.

Implications:

- a slow live plugin reduces result cadence, but it no longer directly stalls
  egui repaint;
- plugins inside one phase run **sequentially**, in registration order, and
  the host does not parallelize them.
- plugins cannot legitimately spawn their own threads and call host
  callbacks from those threads, because the callback `ctx` pointer is only
  valid for the duration of the current `process_frame` call.
- current-frame raw-event materialization is paid only for enabled
  `RawEvents` plugins;
- live host-view dataset publication is worker-snapshotted and then rendered on
  the GUI thread.
  copies them, and decodes into `Arc<TableDatasetV1>` etc.

The host's own built-in tools (`HotpixelDetection`) run in the same loop and
are subject to the same constraints.

## How Replay Plays Back

A few replay specifics that affect threading:

- raw `.raw` replay (`RawFileCamera`) reuses the steady-state pipeline
  unchanged. The USB thread becomes a file-reader thread, and replay timing
  (`throttle_to_current_progress`, `replay.rs:308`) sleeps **on that
  thread**. Pause toggling is just an `AtomicBool` flip from the GUI.
- decoded replay (`DecodedEventFileCamera`) is opened off-thread to avoid
  freezing the GUI; once opened it follows the same pipeline pattern. The
  shared `Arc<Vec<CdEvent>>` is read by the file-reader thread and also held
  by `CameraApp::replay_decoded_events` for export and seek.
- replay seek is initiated by the GUI thread (it pokes
  `current_timestamp_us` and unsets `paused` momentarily) and is observed by
  the file-reader thread on its next iteration. Plugin retained history and
  the 3D cloud catch up only when the targeted display frame is finally
  emitted (the dataflow doc's "during seeking, raw 3D and plugin history can
  diverge" caveat is purely a consequence of where the work runs).

## Hot Paths And Bottlenecks

In rough descending order of risk:

### 1. Per-event accumulation in `preview_thread`

`pipeline.rs:1297` is the inner loop over all decoded events. It already
includes:

- bounds check;
- index compute + `pixels` increment + total-histogram bin transition;
- on/off saturating add;
- signed-histogram bin transition;
- conditional `frame_events.push(*ev)`.

At 100M events/s the loop body is the dominant cost. The `transition_*`
helpers and the optional event copy are the two places where unnecessary
work shows up.

### 2. Live plugin pipeline

The live worker is the main runtime plugin path:

- it coalesces queued frame triggers to the newest frame;
- retained-history plugins drain all events through a dedicated upstream cursor;
- results carry an epoch and stale results are dropped by the GUI;
- live outputs are labeled approximate because result cadence may lag.

Replay recomputation can still run synchronously, where determinism and
single-step behavior matter more than live responsiveness.

### 3. `LiveEventSource` mutex contention

`Arc<Mutex<EventRing>>` is the only mutex on the per-frame-emission path:

- locked by the preview thread on `emit_preview_frame`;
- locked by the GUI thread inside `events_snapshot`, `events_for_range`,
  `materialize_frame`, and `drain_cursor_frames`.

Today the lock is held briefly, but it serializes the preview thread with
plugin retained-history materialization. Long plugin retained-history
walks would extend the GUI-side critical section.

### 4. `update_preview_texture` drain loop

For every drained frame (not just the newest) the GUI thread calls
`viewer.workspace.point_cloud.push_frame(&frame)`. This is cheap (the cloud
stores upstream source + range, not events) but it does scale with the queue
depth seen between repaints. If `update()` is starved, the queue caps at 4
frames, so this is naturally bounded.

### 5. CPU staging in renderers

Both `preview_renderer.rs::render*` and `inspection_3d.rs::prepare_scene_points
+ render` rebuild CPU-side staging vectors each repaint. Persistent GPU
buffers grow on demand but the CPU staging vectors are reallocated. This is
fine at "normal" event counts but is the first thing that bites if a user
asks for a very long 3D time window.

### 6. Synchronous WGPU readback

`map_buffer_sync` is the only place the GUI thread waits on the GPU. Calling
it for every preview frame would couple repaint cadence to GPU latency.
Today it is only invoked from the WGPU histogram and hover paths in the
preview compute pipeline.

## What Is *Not* Threaded (And Why That's OK Today)

- **Hotpixel detection** runs on the GUI thread. It operates on a single
  `PreviewFrame` and is bounded by sensor pixel count, not by event rate.
- **Host-view dataset decoding** runs on the GUI thread. The plugin-supplied
  byte payloads are typically modest in size; if a plugin published a very
  large table, this would become a hot spot.
- **Replay file decoding for `.csv` / `.h5` / etc.** runs once on the
  one-shot replay-open thread; replay playback after that does not redecode.
- **TIFF export** runs on its own one-shot thread (`app.rs:4081`).

## File Guide

| File | Threading role |
|---|---|
| `augur-core/src/pipeline.rs` | spawns USB, preview, and disk threads; defines all bounded crossbeam channels and atomics |
| `augur-core/src/replay.rs` | raw replay; runs on the USB thread, owns the wall-clock throttle and pause atomics |
| `augur-core/src/decoded_replay.rs` | decoded replay; runs on the USB thread but the heavy decoding happens once on the replay-open helper thread |
| `augur-gui/src/render_backend.rs` | starts the eframe/egui main loop and configures the WGPU backend |
| `augur-gui/src/app.rs` | the single GUI thread; owns queue drain, live-worker dispatch/result publish, replay/export helper threads, and scene assembly |
| `augur-gui/src/preview.rs` | thread-local preview scratch (GUI thread only) |
| `augur-gui/src/preview_renderer.rs` | WGPU compute + render pipelines for the 2D preview, encoded on the GUI thread |
| `augur-gui/src/inspection_3d.rs` | WGPU 3D renderer, encoded on the GUI thread, output exposed as a native texture |
| `augur-runtime/src/lib.rs` | `libloading` plugin host, retained history, FFI bridges, and live analysis worker |
| `augur-gui/src/plugin_loader.rs` | compatibility re-export of `augur-runtime` plugin loader types |
| `augur-gui/src/external_tools/imagej.rs` | TCP bridge worker thread + GUI-side `try_send` channel |

## Verification

- Thread spawns audited:
  - `pipeline.rs:1029`, `pipeline.rs:1205`, `pipeline.rs:1257`
  - `app.rs:3889`, `app.rs:4081`
  - `external_tools/imagej.rs:87`
  - `eframe::run_native` in `render_backend.rs:83`
- Channel capacities and policies cross-checked against the constants near
  `pipeline.rs:33–38`.
- Mutex / atomic ownership audited in `pipeline.rs`, `replay.rs`,
  `decoded_replay.rs`, `external_tools/imagej.rs`, and `preview.rs`.
- Plugin call site confirmed at `app.rs:4851` and `plugin_loader.rs:602`.
- Cross-check against `Cargo.toml`s confirms no `tokio` / `rayon` / `async-*`
  runtimes are linked, so the model above is exhaustive for steady-state
  threading.
