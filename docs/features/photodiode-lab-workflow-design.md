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

## Suggested order

Freeze toggle (host, small) → raw export action (plugin, small) → cursors +
stats (host, medium) → phase markers (firmware 0.5.0 + plugin, medium) →
overlay view ADR (host, large).
