# Viewer Toolbar And Status Layout

## Summary

`augur-gui` now follows the prototype viewer hierarchy for both the shared 2D preview and the host
3D investigation pane:

- the viewport itself
- a compact `Display` strip for rendering-specific controls
- the replay transport directly below the display strip when replay is active
- a compact footer for live status plus expandable diagnostics

The 2D viewer keeps editing/navigation tools above the canvas, while display controls, replay
transport, and diagnostics now live below the image so the data viewport gets first priority.

## Behavior

- The 2D viewer toolbar keeps pointer/ROI/measurement/annotation tools, zoom controls, crop,
  histogram, and popup actions.
- The 2D `Display` strip now owns:
  - preview mode selection
  - scale-bar visibility and position
  - time-surface decay tuning, visible as a stable control in the strip
  - annotation count ("Annotations: N shape(s)")
- Replay controls are composed by the viewer itself in 2D-only and split layouts, so main and popup
  viewers use the same row order: header, toolbar, canvas, display strip, transport, footer.
- In split mode, the header and 2D toolbar are shared across the central viewer column; only the
  actual 2D and 3D viewport band is split.
- The 2D footer now shows a one-line summary with preview throughput (□ Mev/s · ↑ MB/s · ◯ elapsed),
  frame ON/OFF balance, hover readout, and ruler measurements. Expanded diagnostics keep pipeline stats, runtime-dirty state,
  analysis warnings, notices, hotpixel masking actions, replay-open progress, and errors.
- The same footer line carries the live **sensor conditions** — scene illumination and die
  temperature — because a bias setting only means something against the light level and
  temperature it was chosen under, and this is the one place both are readable without opening
  the settings sidebar. `sensor_footer_readings` drops the pair entirely once the readback goes
  stale (`settings::MONITORING_STALE_AFTER_S`) or the last poll failed: a frozen light level
  sitting beside live throughput numbers would read as current when it is not. The full readout,
  including measured dead time, stays in Camera Settings → Sensor Readout.
- The viewer head no longer prints a `1 2D  2 Split  3 3D` hint. The pill cluster in the menu bar
  is the control itself and carries each shortcut in its hover text.
- The 3D canvas now overlays compact `ISO` / `XY` / `XT` / `YT`, reset, fit, and focus controls
  directly on the viewport instead of reserving a tall text toolbar above it.
- In split mode, point-size, raw-history, and max-point controls also live in that canvas overlay
  so the user does not scroll a separate right-pane strip just to tune 3D context.
- The 3D overlay keeps one compact horizontal control row for:
  - point scale
  - raw-event history range
  - raw-event max-point budget
- The 3D footer now summarizes visible layers, point count, retained history, and active focus
  volume, with expandable orientation and control guidance below.
- Both display strips stay open unconditionally; the toggle UI and its associated state field have
  been removed.
- The split divider spans only the viewport band between shared chrome rows. It no longer runs
  through the header, toolbar, display strip, replay transport, and diagnostics footer.
- The 2D canvas keeps a minimum vertical budget in tight split panes so the toolbar/display strip
  cannot consume the whole pane and leave only a blank sliver.
- In replay/read-only viewer paths, the 2D canvas auto-fits the active hardware ROI. If replay
  metadata does not expose a tight ROI, it falls back to a padded active-pixel bound so inactive
  black frame margins do not dominate split-mode previews.
- The replay transport is shown unconditionally whenever `AppMode::Replaying` is active, even when
  `total_duration_us` is not yet known from the file header. When duration is unknown, the seek
  slider is hidden but play/pause/stop and the byte-progress readout remain visible.
- The replay transport keyboard shortcuts (Space, ←/→) are also active whenever replay mode is
  active, regardless of whether the file duration was scanned.
- Switching from a 3D-only layout back to split or 2D-only refreshes the 2D preview texture from
  the latest decoded frame when one is already available.
- Main, split, and popup viewers use stable per-pane ID scopes for toolbars, display strips, and
  split panes to avoid duplicate egui widget IDs.

## Files

| File | Role |
|---|---|
| `augur-gui/src/viewer_widget.rs` | shared 2D toolbar/display-strip/footer composition and diagnostics |
| `augur-gui/src/inspection_3d.rs` | 3D toolbar/display-strip/footer composition and retained-history status |

## Verification

```bash
cargo fmt --all
cargo check -p augur-gui --bin AugurRS
cargo clippy --workspace
cargo test --workspace
```
