# Design investigation: hands-on lab workflow for live plugin signals

Status: **proposal** (2026-07-16) — investigation requested for the Stage-A bench
work; nothing below is implemented unless marked so. Companion changes that
already landed: host-view live repaint fix (this repo), PDA1 binary photodiode
stream at 20 kSa/s (`stage-a-controller` 0.4.0, ADR 003 there), plugin-side
envelope decimation + moving-average indicator (`augur-plugins`).

## 1. Raw data export with honest timestamps

**What exists.** The photodiode plugin now retains up to 130 s of raw ADC codes
keyed by *device sample index* at a known rate; host-view tables already export
CSV. Series windows have no export, and the plugin's full-resolution ring is
not reachable from the GUI.

**Time base.** The only honest clock for the photodiode samples is the Teensy's
own sample counter: `t = first_sample_index / sample_rate_hz` within a segment.
Host receive time adds USB jitter (~ms) and should only ever be recorded as an
*anchor* (`host_monotonic_at_last_frame`), never per sample. Alignment to the
camera timeline should reuse the EXT_TRIGGER path (ADR 026): one shared
hardware edge into both the camera and the Teensy marker input beats any
software clock mapping.

**Proposal.**
- Plugin-side "Export raw data" host action (the registry already supports
  actions): dump the current ring as CSV (`segment, sample_index, t_s, code,
  volts`) plus a JSON sidecar (rate, mode, reference, firmware version, drop /
  CRC counters, host anchor). `stage-a-io` already has `PdqWriter` +
  `RunSidecar` for the binary variant.
- Host-side: add CSV export to `LineSeriesWindow` views (exports what the
  plugin published — the decimated series — clearly labeled as such).

## 2. Overlay: photodiode vs. commanded modulation

**Problem.** The commanded DAC value lives in `stage-a-modulation`; the
photodiode samples live in the Teensy stream. Correlating them via host clocks
is guesswork.

**Key insight: both signals originate on the same Teensy.** The firmware can
anchor them exactly:
- Firmware 0.5.0 proposal: the MOD engine emits a `Marker` frame on the stream
  port at every waveform phase zero-crossing (the marker machinery and the
  `sample_index` field already exist in the wire format). The photodiode plugin
  then knows the modulation phase at every sample and can render the *commanded*
  waveform (shape, level, min, frequency are readable from the persistent
  context bus or entered by the user) as a second line — perfectly aligned, no
  host clocks involved.
- Alternative without firmware change (approximate): the modulation plugin
  publishes its commanded waveform parameters on the context bus; the
  photodiode plugin fits the phase by correlation over the visible window.
  Fine for visual overlay, not for metrology.

**Host generalization (bigger, later).** A generic "overlay view" that renders
several `Series1d` datasets from different providers on one axis pair would
serve every future two-signal case; needs an ADR (dataset relations already
exist in the registry descriptors as a starting point).

## 3. Pause / freeze for inspection

**Observation.** Host-view datasets are generation-cached; the GUI re-renders
only when the provider generation changes. Freezing the *display* is therefore
cheap and safe: keep acquiring into the plugin rings, stop advancing the
rendered snapshot.

**Proposal.**
- Per-window **Freeze** toggle on host-view windows (and dock tabs): while
  frozen, the host ignores new generations for that view; the plot becomes
  fully inspectable (egui plot zoom/pan already works) and CSV export uses the
  frozen snapshot.
- A global "Freeze live views" toggle for multi-source work: freezes all
  host-view snapshots in the same GUI pass, giving a *consistent* cross-plugin
  cut (within one worker snapshot — the strongest consistency available
  host-side).
- Data is not lost while frozen: the photodiode ring holds 130 s; resuming
  jumps to "now". If longer frozen inspection is needed, export first — that
  matches scope-style workflows (RUN/STOP + save).

## 4. Further researcher-facing gaps (ranked)

1. **Cursors + window statistics** on series plots: pointer readout, two
   x-cursors with Δt/Δy, and mean/σ/Vpp/min/max of the visible window — the
   moving-average readout already covers the "current value"; this covers
   "measure what I see".
2. **FFT / spectrum view** of the visible photodiode window (20 kSa/s makes
   this immediately useful for verifying the modulation frequency and spotting
   mains hum in the low-voltage regime).
3. **Absolute-time x-axis option** (segment-relative seconds) instead of
   "seconds before now", so a frozen plot doesn't imply motion.
4. **PNG export** for series windows (density/image views already have image
   export paths).
5. **Record photodiode stream alongside camera recordings** (.pdq + sidecar
   next to the .raw), so analysis runs can correlate offline.

## 5. Path to 500 kSa/s – 1 MSa/s (ADC/DMA backend)

Decisions from the 2026-07-17 review: 20 kSa/s is not enough; target the
ADC/DMA backend. Overlay alignment will use a hardware EXT_TRIGGER from the
Teensy to the camera (no phase-marker firmware needed for that use).

**How the stream works today (0.4.0).** A PIT `IntervalTimer` ISR fires at the
sample rate and calls blocking `analogRead()` (~2–3 µs); samples fill
fixed-size blocks in a ring; the foreground loop packages ready blocks into
CRC32 PDA1 frames and writes them non-blockingly to the second USB CDC port.
Timing: the sample *instant* is the ISR entry — nominally period-exact from
the hardware timer but software-jittered by interrupt latency and by the MOD
engine's SPI ISR (up to 40 kHz). The authoritative clock is the sample index;
`first/last_tick_us` per block let the host audit the real cadence. This
portable backend saturates around 100 kSa/s and burns ~25–30 % CPU there.

**Backend plan (firmware 0.5.0).**
- Hardware-triggered conversions: QTimer/PIT → ADC_ETC → ADC1 in 12-bit
  high-speed mode, results moved by **DMA** into a double-buffered ring in
  RAM2 (`DMAMEM`, with dcache invalidation). Sample spacing becomes exactly
  the hardware timer period — zero ISR jitter, near-zero CPU in the sampling
  path; the CPU only packages frames per half-buffer completion.
- Rate: 500 kSa/s comfortable, ~1 MSa/s at the ADC spec limit (expect
  ~10.5–11 ENOB there; verify against the front end). Pin 18/A4 is reachable
  from both ADCs, so two-ADC interleaving to ~2 MSa/s stays open later.
- Frames: grow `samples_per_block` (256 → 2048) at high rates so header+CRC
  overhead stays ≈1 %; wire format is unchanged (`sample_rate_hz` header
  field already carries any rate). USB HS moves 2 MB/s without strain, but
  switch the CRC to a table-driven implementation at these rates.
- Side effects to check on the bench: MOD SPI ISR vs. DMA interrupt priority;
  RAM2 budget for the DMA ring; and — the real new limit — the **analog front
  end**: transimpedance bandwidth and an anti-aliasing RC sized for the new
  Nyquist, otherwise HF noise folds into the low-voltage signal.
- Host side at ≥500 kSa/s: the plugin's per-repaint full-window rescan must
  become an **incremental min/max/sum pyramid** maintained on ingest
  (e.g. 64:1 and 4096:1 levels), and the cache length becomes a setting
  (default 20 s; 1 MSa/s × 20 s = 40 MB of codes) instead of a fixed 130 s.

**Verdict on "is streaming raw the best approach":** yes for this system —
USB HS affords raw up to ~2 MSa/s, keeping firmware dumb and every sample
available to the host for caching/recording. On-device decimation only pays
off beyond that or for multi-channel.

## 6. Saving: monitor cache vs. recording (approved direction)

Two modes, one lean "Data" settings section, shared writer (`stage-a-io`'s
`PdqWriter` + `RunSidecar`, plus CSV for quick plots):

- **Monitor mode (always on):** rolling cache of `cache_s` seconds (default
  20 s, the existing ring) — one **Save cache snapshot** control dumps it.
- **Recording mode:** explicit start/stop; the reader thread tees frames to
  disk from the moment recording starts, so length is disk-bound, not
  RAM-bound. Auto-named files (`pd_YYYYmmdd_HHMMSS.pdq` + `.json` sidecar +
  optional `.csv`) in a user-chosen data directory.

**Gap to close first:** plugin settings today have no text/path/button kinds,
and host actions are only delivered inside `process_frame` — which never runs
for the camera-less Stage-A bench. Plan: extend `SettingKind` with `Text`,
`Path` (host renders a native file dialog), and `Button` (momentary trigger
routed through `set_setting`, which *is* frame-independent). This one API
addition serves the recorder UI, the protocol file below, and every future
device plugin.

## 7. Protocols and cross-plugin / plugin-host control

- **Protocol into stage-a-modulation (approved):** don't build cross-plugin
  remote control for this — put the protocol executor *inside* the modulation
  plugin, which already owns the command port and a worker thread. A TOML
  protocol file (steps: `t`, `wave`, `level`, `min`, `freq_mhz`, loop count)
  loaded via the new `Path` setting (interim: an enum of files found in a
  `protocols/` directory, the same pattern as the port enum), run/stop
  controls, progress published as a table dataset. Host-OS step timing is
  ±ms; if a protocol ever needs µs-exact steps, the step table moves into
  the firmware as a new command (later ADR).
- **Plugin → plugin control (general):** today the only channel is the
  persistent context bus, and it is delivered per frame — unusable without a
  camera. The architecture's intended pattern (ADR 006 in augur-plugins) is
  *ownership*, not puppeteering: a future experiment plugin links
  `stage-a-io` and owns the command port itself (modulation plugin disabled
  while the experiment is armed). A host-routed `set_setting` request API
  (plugin asks host to set another plugin's setting) is feasible and
  frame-independent; the design — single-writer arbitration, "manual wins",
  and a mandatory "driven by" badge — is written up in **ADR 027**
  (proposed).
- **Plugin → host application control (recording, filenames):** deliberately
  impossible today (fail-closed `ExecutionContext`, ADR 026). The design for
  an explicit, allowlisted host-command queue mirroring `HostActionRequest`
  in reverse — a closed verb set (`StartRecording { name }` /
  `StopRecording`), per-plugin consent, sanitised filenames confined to the
  data directory, and visible execution — is in **ADR 028** (proposed).

## Suggested order (updated 2026-07-17, approved items marked ★)

1. ★ Freeze toggle (host, small) — discussed & approved
2. ★ SettingKind `Text`/`Path`/`Button` (API + host, small-medium) —
   unblocks recorder UI and protocol loading
3. ★ Monitor-cache save + recording mode in the photodiode plugin (medium)
4. ★ Cursors + window statistics (host, medium)
5. ★ FFT/spectrum view, absolute-time axis, PNG export (host, medium)
6. Protocol executor in stage-a-modulation (plugin, medium)
7. ADC/DMA backend firmware 0.5.0 → 500 kSa/s–1 MSa/s (+ plugin pyramid
   decimation + cache-length setting) (large)
8. ADRs: plugin→plugin setting requests; plugin→host command queue (design)
