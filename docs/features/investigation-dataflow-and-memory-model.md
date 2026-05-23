# Investigation Dataflow And Memory Model

## Summary

This note documents how the current investigation stack in `augur-rs` moves
data from capture or replay into:

- the 2D preview image
- the host-owned 2D investigation markers
- the 3D raw-event cloud
- the plugin-facing retained event history exposed as `EventStoreHandle`
- plugin-provided table/image/series datasets and host views

The short version is:

- the **2D preview image** is rendered from the latest `PreviewFrame`'s
  accumulated pixel planes
- the **3D raw-event cloud** is rendered from GUI-owned projection metadata in
  `PointCloudState`; it stores upstream source handles and frame ranges, then
  materializes sampled events only for the renderer
- the **plugin retained history** is another projection: `PluginEventHistory`
  registers a dedicated lossless upstream cursor, copies complete frame windows
  from that cursor under the existing memory budget, and materializes
  `FfiCdEvent` slices when an `EventStoreHandle` callback is invoked
- the **host-view datasets** are a fourth path: plugins serialize datasets to
  bytes, the host copies those bytes, decodes them into cached snapshots, and
  then builds 2D/3D investigation layers from those snapshots

So yes: the current implementation still has multiple histories, but the raw
3D cloud and GUI-hosted plugin history are now projections over the upstream
event source instead of independent raw-event owners.

The migration target is now documented in ADR 020:

- one upstream raw-event `EventSource` receives decoded `CompactEvent`s before
  the bounded preview-frame channel
- recording and runtime plugins read from that upstream source through
  lossless cursors
- the 2D preview, 3D raw-event cloud, and replay UI become lossy projections
  that keep only derived caches such as count planes or GPU sample buffers
- host-view datasets stay in `host_views.rs`; the raw-event ring unifies raw
  events only, not plugin-provided tables/images/series

The upstream ring, GUI projection, dedicated plugin-history cursor, decoded
replay `EventSource`, and cold-scan raw replay `EventSource` are in place. The
remaining target gap is indexed raw replay / recording-file repagination as
described in ADR 020.

## Existing Documentation Audit

The current docs already cover important slices of the system:

- [Replay](./raw-replay.md)
- [WGPU Preview Rendering](./wgpu-preview-rendering.md)
- [Investigation Workspace](./investigation-workspace.md)
- [Plugin Authoring Guide](./plugin-authoring-guide.md)
- ADR 007, 008, 015, and 016 in `docs/adr/`

What was missing before this note was an end-to-end explanation of ownership,
allocation, and the boundaries between those subsystems.

### Current accuracy

- `docs/features/investigation-workspace.md` is broadly up to date about the
  host-owned layout and linked-selection model.
- `docs/features/raw-replay.md` is broadly up to date about replay mechanics.
- `docs/features/plugin-authoring-guide.md` is broadly up to date about the
  runtime plugin contract.

### Stale or incomplete points

- `docs/features/interactive-preview-workbench.md` still reflects the older
  transition period where `point_cloud.rs` was described as the full 3D
  renderer. Today it is only the raw-event history buffer; rendering lives in
  `augur-gui/src/inspection_3d.rs`.
- `docs/gui.md` previously implied that 3D plugin data comes from timestamped
  marker overlays or a `coordinate_columns_3d` contract. The current 3D plugin
  path is `HostViewKind::Scatter3dFromTable` backed by table datasets.

## End-To-End Runtime Flow

### 1. Decode and accumulate in `augur-core`

`augur-core/src/pipeline.rs` owns the capture/replay preview pipeline.

For each decoded batch of `CdEvent`s, the preview thread:

1. updates pooled per-pixel accumulators:
   - `pixels`
   - `pixels_on`
   - `pixels_off`
   - cached histograms
2. appends the closed frame's raw events into the upstream `LiveEventSource`
   / `EventRing`
3. optionally keeps an inline `PreviewFrame.events` copy when
   `raw_events_needed == true`
4. emits a `PreviewFrame` once the acquisition window closes

`PreviewFrame` contains:

- image-sized count planes
- cached histograms
- optional `events: Option<Vec<CdEvent>>` compatibility cache
- optional `event_range: Option<Range<u64>>` plus `event_source` handle for
  resolving raw events from the upstream ring
- `window_start_us` / `window_end_us`

The image/count buffers are pooled and recycled on `Drop`. The raw-event
`Vec<CdEvent>` is not pooled the same way; it is moved into the frame only when
raw events were requested. The upstream ring receives the frame before the
bounded preview-frame queue can drop the UI projection.

### 2. Drain preview frames in `augur-gui`

`CameraApp::update_preview_texture()` drains the preview-frame queue and keeps:

- **all drained frame ranges** for the GUI raw-history projection
- **the newest drained `PreviewFrame` candidate** for 2D rendering, plugin
  execution, histograms, line profile, and replay snapshots

That single choice is one of the most important current design facts:

- the 3D raw-event view can see data from multiple drained frames
- the 2D preview and plugins only process the newest frame that survived the
  drain

If the GUI throttles processing for display cadence, that newest candidate is
kept in `pending_preview_frame` instead of being dropped. This keeps the
intended asymmetry between 3D continuity and 2D current-frame processing
without allowing the 3D raw-history path to advance while the 2D preview never
receives a display frame.

Runtime plugins that request retained event history have one extra drain-time
requirement: `update_preview_texture()` synchronizes the plugin `EventStore`
from the upstream `plugin-runtime` cursor as soon as preview frames are
dequeued. Plugin analysis can still run only on the newest processed frame, but
the lossless upstream cursor advances before display throttling, click-triggered
recomputes, or replay snapshot application can leave it pinned at an old
logical event and block ring eviction.

## Allocation And Ownership Map

### `PreviewFrame` buffers

Owner: `augur-core`

- `pixels`, `pixels_on`, `pixels_off`, and cached histograms come from a small
  global pool
- those buffers are reused across frames
- `PreviewFrame::drop` returns them to the pool

This is the most allocation-aware part of the pipeline today.

### `PreviewFrame.events`

Owner: `augur-core`, then `augur-gui`

- allocated only when `raw_events_needed` is true
- moved into the emitted `PreviewFrame`
- later materialized through `PreviewFrame::events_snapshot()` only for
  compatibility consumers that still need a vector
- kept temporarily for compatibility while GUI consumers migrate to
  `PreviewFrame::events_snapshot()`

This raw-event vector used to be the branching point for most later
duplication. During the upstream-source migration, `PreviewFrame.event_range`
is the preferred identity for the raw events represented by a frame.

### `LiveEventSource`

Owner: `augur-core`, shared with `augur-gui` through cloned handles

- wraps the upstream `augur_event_types::EventRing`
- stores decoded frame events before the bounded preview channel can drop a
  `PreviewFrame`
- exposes logical event ranges to `PreviewFrame::events_snapshot()`
- registers a recording cursor in recording sessions; this cursor is advanced
  after each append because the raw-byte disk writer has already received the
  packet stream independently

This is the first landed piece of ADR 020. Runtime plugins still receive the
current compatibility snapshots in this phase, but the source of those
snapshots can now be upstream of preview-frame drops.

### `latest_frame`

Owner: `augur-gui::CameraApp`

- stores the newest processed `PreviewFrame`
- is the authoritative source for the displayed 2D preview image
- is also the frame passed into hotpixel detection and runtime plugins

### `PointCloudState`

Owner: `augur-gui`

- type: `VecDeque<RetainedEventFrame>` projection metadata
- filled from **all drained `PreviewFrame` event ranges**, not just the newest
  frame
- trimmed by time and point-count limits, not by the plugin memory budget

This is no longer a raw-event owner. It stores upstream source handles and
logical frame ranges, then materializes a sampled `Vec<CdEvent>` only when the
3D scene asks for visible events. It remains independent from the plugin
retained-history projection.

### Plugin retained history / `EventStoreHandle`

Owner: `augur-gui`, implementation in `augur-gui/src/plugin_loader.rs`

- type: `VecDeque<PluginEventFrame>`
- each stored frame normally keeps a `LiveEventSource` handle plus a logical
  event range
- fallback inline storage exists only for compatibility frames that do not
  carry an upstream source/range
- limited by `event_store_budget_bytes`
- only appended from the newest processed frame when
  `append_current_frame_to_event_store == true`

This is the retained-history source for plugins. The ABI-facing
`EventStoreHandle` remains unchanged: plugins still request frame counts,
specific frames, timestamp ranges, and the oldest timestamp. The host resolves
those requests by materializing `FfiCdEvent` frame slices on demand from the
upstream source.

`augur-plugin-api/src/event_store.rs` remains as the public helper and
compatibility implementation for plugin-side tests, but it is no longer the
GUI host's runtime retained-history owner.

### Host-view dataset cache

Owner: `augur-gui`

- plugin exposes descriptors via `host_views()`
- plugin returns serialized bytes via `host_view_dataset(dataset_id)`
- host copies those bytes into `Vec<u8>`
- host deserializes them into `Arc<TableDatasetV1>`, `Arc<Image2dV1>`, or
  `Arc<Series1dV1>`
- cache lifetime is controlled by plugin-reported
  `host_view_dataset_generation(dataset_id)`

This is the source for host-rendered tables, 2D investigation points, density
maps, scatter plots, and plugin-provided 3D scatter layers.

### Preview-renderer scratch and GPU state

Owner: `augur-gui`

- CPU time-surface scratch lives in thread-local `PreviewRenderScratch`
- WGPU preview rendering keeps persistent textures/buffers
- the WGPU 3D renderer keeps persistent render targets plus a grow-on-demand
  instance buffer

These are presentation-side copies/derivations, not authoritative data
stores. A semantic `Investigation3dScene` cache is intentionally not part of
this model: the scene must be rebuilt from current viewer state, raw-history
projection metadata, and host-view dataset generations so ROI, layer, selection,
and history-control edits are visible without waiting for another frame.

## How The 2D Scene Is Rendered

There are really three 2D layers:

### 1. The base preview image

Source:

- `latest_frame.pixels`
- `latest_frame.pixels_on`
- `latest_frame.pixels_off`
- optionally `latest_frame.events` for time-surface and some WGPU preview modes

Path:

1. `CameraApp::render_preview_texture_payload()`
2. `preview.rs::with_prepared_preview_frame(...)`
3. `preview_renderer.rs`
4. `PreviewDisplayTexture`
5. `viewer_widget.rs` paints the resulting texture

Important detail:

- normal 2D preview is count-plane based
- it is not rendered from the 3D raw-history buffer
- time-surface is reconstructed through `PreviewFrame::events_snapshot()` when
  upstream raw events are available; otherwise it falls back

### 2. Overlay layers

Source:

- `analysis_output.overlays`

Produced by:

- host-owned tools such as hotpixel detection
- runtime plugins through `HostOutput`

These are painted on top of the preview image in `viewer_widget.rs`.

### 3. Host-owned 2D investigation points

Source:

- cached plugin datasets with `TableSchema.coordinate_space_2d`

Path:

1. `CameraApp::build_investigation_points_2d()`
2. filter rows by active ROI and current frame span
3. convert rows into `Investigation2dPoint`
4. paint them in `viewer_widget.rs`

These points are not derived from raw preview events. They are derived from
plugin datasets cached in the host-view system.

## How The 3D Scene Is Rendered

The 3D scene has two independent data families.

### 1. Raw-event layers

Source:

- `viewer.workspace.point_cloud.visible_events()`

Those events came from:

- drained `PreviewFrame.event_range` values
- retained in `PointCloudState` as source/range projection metadata
- materialized into a sampled temporary `Vec<CdEvent>` when `visible_events()`
  is called

Path:

1. `update_preview_texture()` drains all queued frames
2. all drained frame projections are appended into `PointCloudState`
3. `build_investigation_scene_3d()` pulls `visible_events()`
4. each event becomes an `Investigation3dPoint`
5. `inspection_3d.rs` prepares GPU instance data and uploads it

`build_investigation_scene_3d()` is deliberately a projection builder rather
than a cache lookup. It may allocate temporary scene vectors, but it must not
retain a semantic scene snapshot across frames or UI edits.

Coordinate mapping:

- `x` stays sensor `x`
- `y` is flipped so 3D matches the 2D preview orientation
- `z` is event age relative to the newest visible raw event

### 2. Plugin-provided 3D scatter layers

Source:

- cached table datasets referenced by `HostViewKind::Scatter3dFromTable`

Path:

1. `build_investigation_scene_3d()` iterates visible `Scatter3dFromTable`
   views
2. it loads the cached table snapshot for that dataset id
3. it filters rows by ROI and current frame span
4. it converts each row into an `Investigation3dPoint`
5. the WGPU 3D renderer uploads those points alongside the raw-event layers

Important detail:

- marker overlays do **not** currently become 3D points
- the current plugin 3D path is table-driven, not overlay-driven

### 3. GPU upload path

`inspection_3d.rs` rebuilds CPU-side render vectors each render:

- `prepare_scene_points(...)` allocates a fresh `Vec<PreparedPoint>`
- render then allocates a fresh `Vec<PointInstanceRaw>` for GPU upload
- the GPU instance buffer itself is persistent and only grows when capacity is
  insufficient

So the 3D renderer is allocation-aware on the GPU-resource side, but it still
rebuilds per-frame CPU staging vectors. Persistent caches are limited to
presentation resources; they are not allowed to decide which points, layers,
ROI focus volume, or raw-history window are visible.

## Do 2D And 3D Share The Exact Same Data?

No, not in the strict sense.

### What is shared conceptually

- the base 2D preview image and the raw 3D cloud both originate from the same
  decoded `CdEvent`s when raw-event capture is enabled
- a plugin dataset can feed both 2D and 3D host views if the plugin publishes
  both coordinate spaces/views for the same dataset

### What is not shared physically

- the 2D preview image uses accumulated count planes
- the raw 3D cloud uses upstream frame ranges and samples materialized events
  for rendering
- plugins use upstream frame ranges through `PluginEventHistory`, with
  `FfiCdEvent` slices materialized only during `EventStoreHandle` callbacks
- host views use decoded dataset snapshots built from serialized plugin bytes

### What is not even the same temporal slice

- 2D preview + current-frame plugin execution operate on the newest processed
  display frame
- raw 3D history can include intermediate drained frames that never became the
  visible 2D frame, but visible raw 3D rendering is anchored to the displayed
  2D frame's `window_end_us`

That means the raw 3D cloud can be temporally richer than both the 2D preview
and current-frame plugin execution, but it should not run ahead of the 2D
preview timestamp. If the 2D frame covers `[t-x, t]` and the 3D history control
is set to `y ms`, the raw 3D cloud is selected from `[t-y, t]`.

## Replay And Seeking Behavior

### Replay sources

- raw `.raw` replay uses `RawFileCamera`
- decoded `.csv` / `.bin` / `.npy` / `.h5` replay uses
  `DecodedEventFileCamera`

### Replay allocation model

Raw replay:

- reopens the file at aligned byte offsets
- does not keep the whole file decoded in memory

Decoded replay:

- decodes the entire file once into `Arc<Vec<CdEvent>>`
- seek/reopen reuses that shared event vector
- each emitted replay frame can still create a new per-frame raw-event vector,
  but the GUI point cloud and plugin retained history keep upstream ranges
  rather than owning additional raw-event copies

So decoded replay is seek-friendly, but it has the highest number of live
representations of the same event data.

### Replay seeking details that matter for investigation state

- paused seek/forward-step can keep decoding until the requested target frame
  is actually reached
- while waiting, drained raw events are already appended into the upstream event
  source and can feed point-cloud projections
- plugin analysis and plugin retained history are only updated when the chosen
  display frame is finally processed

This again means raw 3D history and plugin history can diverge during seeking.

### Replay frame history snapshots

Backward stepping can restore a cached `ReplayFrameSnapshot` instead of
reopening the replay source.

That snapshot restores:

- the displayed `PreviewFrame`
- the cached `analysis_output`
- the preview histogram
- a point-cloud history rebuilt from `snapshot.frame`'s upstream source/range

When a snapshot carries an upstream source/range, it intentionally drops the
temporary inline `PreviewFrame.events` cache. Restoring the snapshot can still
resolve raw events through `PreviewFrame::events_snapshot()`.

It does **not** rewind plugin retained history.

That remains an important state-coherence caveat: when the user is looking at
an older snapshot, the visible 2D frame, visible 3D cloud, and saved plugin
outputs can all reflect the snapshot, while plugin retained history may still
reflect the later controller state.

## Plugin Interface Boundary

At runtime, the host calls each enabled plugin with four main inputs:

1. `PluginFrame`
   - width / height
   - `pixels()` total-count plane
   - `events()` raw current-frame `FfiCdEvent`s
   - frame time span
2. `HostContext`
   - per-frame context bus
   - persistent context bus
   - `raw_events()` access
3. `EventStoreHandle`
   - retained frame count
   - random access to retained frames
   - frame range lookup by timestamp
4. `HostOutput`
   - overlays
   - marker overlays
   - warnings

Separately, plugins may publish declarative host views:

- `host_views()`
- `host_view_dataset(dataset_id)`
- `host_view_dataset_generation(dataset_id)`

This means the plugin boundary itself already has three distinct data paths:

- current frame
- retained raw-event history
- structured host-view datasets

## Observed Refactor Pressure Points

These are observations about the current implementation, not decisions.

### 1. Raw 3D history and plugin history are separate projections

They retain different metadata, under different policies, with different
temporal coverage:

- `PointCloudState`: time-window + point-limit
- `PluginEventHistory`: memory-budgeted frame windows exposed through
  `EventStoreHandle`

ADR 020 resolves this directionally: raw-event indexing moves to the upstream
`EventSource`; GUI 3D state is now a lossy projection, and plugin retained
history is copied from a dedicated lossless cursor upstream of UI frame drops.
The remaining gap is indexed raw replay / recording-file repagination.

### 2. Queue draining favors 3D continuity over single-source consistency

Drained intermediate frames feed the upstream source and can feed the raw 3D
projection. Runtime plugin retained history now catches those frames by draining
its own upstream cursor even when the bounded preview-frame queue drops a GUI
projection. Visible 2D rendering and current-frame plugin execution still use
the newest processed preview frame.

This improves 3D continuity while keeping the visible 3D raw cloud anchored to
the same display-frame end timestamp as the 2D preview. The remaining
"multiple truths" are deliberate projection differences: 2D count planes,
current-frame plugin input, plugin retained history, and sampled 3D raw history
can have different storage and retention windows.

### 3. `FrameOnly` plugins still inherit raw-event marshaling cost

`CameraApp::run_analysis()` currently converts `frame.events` into
`Vec<FfiCdEvent>` whenever any runtime plugin is enabled, even if the enabled
plugins are all `FrameOnly`.

So the raw-event FFI copy boundary is wider than the logical plugin demand.

### 4. Decoded replay is efficient for seeks, but still has a compatibility cache

Decoded replay keeps one shared `Arc<Vec<CdEvent>>`, but later stages still
can still materialize:

- per-frame `PreviewFrame.events`
- renderer/plugin callback staging vectors
- optional plugin-owned datasets

### 5. Raw replay EventSource is correct but not indexed yet

Decoded replay files expose their shared `Arc<Vec<CdEvent>>` through a
timestamp-based `EventSource`. Raw `.raw` replay has a cold-scan
`RawReplayEventSource` for correctness, but it does not yet have sparse `.idx`
checkpoints for efficient arbitrary timestamp-range fetches after the live ring
evicts old windows.

## File Guide

| File | Current role |
|---|---|
| `augur-core/src/pipeline.rs` | preview-frame creation, upstream live event source, buffer pooling, optional raw-event compatibility cache |
| `augur-core/src/replay.rs` | raw EVT3 replay, reopen-at-offset behavior |
| `augur-core/src/decoded_replay.rs` | decoded replay, whole-file shared event vector |
| `augur-event-types/src/lib.rs` | shared compact event type, frame index, event ring, event-source trait, and cursor policy |
| `augur-gui/src/app.rs` | queue drain policy, analysis execution, replay stepping, scene assembly |
| `augur-gui/src/preview.rs` | CPU preview preparation and time-surface scratch state |
| `augur-gui/src/preview_renderer.rs` | CPU/WGPU 2D preview rendering and GPU-side staging |
| `augur-gui/src/point_cloud.rs` | GUI-owned 3D projection metadata over upstream event ranges |
| `augur-gui/src/inspection_3d.rs` | WGPU 3D renderer and interaction |
| `augur-gui/src/host_views.rs` | host dataset/view resolution, decoding, and rendering |
| `augur-gui/src/plugin_loader.rs` | runtime plugin bridge and GUI-hosted retained-history projection |
| `augur-plugin-api/src/event_store.rs` | public retained-event helper / compatibility implementation for plugin-side tests |
| `augur-plugin-api/src/helpers.rs` | safe plugin-side wrappers (`PluginFrame`, `EventStoreHandle`, `HostContext`) |
| `augur-plugin-api/src/context.rs` | host-view registry and dataset/view/action schema |

## Verification

- Code paths traced against the current implementation in:
  - `augur-core`
  - `augur-gui`
  - `augur-plugin-api`
- Targeted tests recommended for spot-checking the documented behavior:
  - replay seek / step behavior in `augur-gui/src/app.rs`
  - replay reopen behavior in `augur-core/src/replay.rs` and
    `augur-core/src/decoded_replay.rs`
  - plugin retained-history callback tests in `augur-gui/src/plugin_loader.rs`
  - compatibility `EventStore` retention tests in `augur-plugin-api/src/event_store.rs`
