# AugurRS Design System

The AugurRS Design System is the single source of truth for chrome
appearance — colours, spacing, radii, status semantics, and the polarity
rendering pair — across the egui-based desktop application. It replaces
the previous bare `egui::Visuals::dark()` / `light()` defaults with a
properly tuned scientific-workbench look while leaving ImageJ LUTs and
existing event/raw rendering data untouched.

## Why

The application is a dense scientific instrument panel. egui's stock
visuals are tuned for general-purpose tooling and produce chrome that is
heavier than the workbench needs. The design system describes a
light-first palette, compact corner radii, hairline separators, and
restrained shadows that fit the "respect the reader, color is for data"
philosophy laid out in the design bundle.

Two further changes were folded in at the same time:

1. The polarity preview no longer ships the generic red/blue cliché.
2. The right analysis panel's `Status & warnings` section condenses the
   verbose body into an always-visible summary plus an opt-in
   `Diagnostics` collapsible.

## What changed

### `augur-gui/src/theme.rs` (new)

- `ThemeMode::{Light, Dark}` plus a `Palette` of resolved chrome colours
  for each mode. Light is the first-class theme; dark is a faithful
  mirror.
- Tokens covering surfaces (`bg_0…bg_4`), lines (`line`,
  `line_strong`, `line_subtle`), text (`fg_0…fg_4`), accent (`accent`,
  `accent_hover`, `accent_press`, `accent_weak`, `focus_ring`) and the
  five status colours (`success`, `info`, `warn`, `error`, `record`).
- Theme-stable data tokens for raw events
  (`RAW_EVENT_ON_RGBA`, `RAW_EVENT_OFF_RGBA`) and the polarity-mode
  rendering pair (`POLARITY_ON_RGB` = `#E83EB0` hot magenta,
  `POLARITY_OFF_RGB` = `#00B8D4` arctic cyan), plus the documented
  ROI/annotation colours.
- 4-based spacing scale (`sp::SP_0`..`SP_8`) and compact radii
  (`radius::R_1`..`R_4`).
- `theme::visuals(mode)` builds a fully populated `egui::Visuals`
  matching the design — panel/window fill, hairline strokes, hover/press
  fills, focus stroke, hyperlink colour, restrained shadows and 6 px
  rounding (the existing `PANEL_ROUNDING`).
- `theme::apply_style(style)` tunes `egui::Spacing` for dense panel
  paddings and the small interactive size used in the workbench.
- `theme::card_frame(ui)` replaces `egui::Frame::group(ui.style())` for
  plugin/layer cards: flat `bg_1` fill, single hairline, 6 px corners.
  The design notes that cards should *not* carry a left coloured bar —
  that pattern was rejected as "AI-looking".
- `theme::section_subhead(ui, text)` renders the terse uppercase
  sub-section headings the design uses inside dense panels.

### `augur-gui/src/app.rs`

- `UiThemePreference::visuals` delegates to `theme::visuals(mode)`.
- `apply_theme_to_ctx` now also applies `theme::apply_style` so spacing
  baselines stick for the whole session.
- `RAW_EVENTS_ON_COLOR` / `RAW_EVENTS_OFF_COLOR` are sourced from the
  theme module so the constants and the documented tokens cannot drift.
- `status_success_color`, `analysis_info_color`, and the new
  `status_record_color` are read from `theme::Palette` rather than being
  inlined RGB literals. The hard-coded `● REC` red was removed in favour
  of `status_record_color()`.
- The right-panel `Status & warnings` block was rewritten:
  - always-visible top line: `Mode: …` plus a "apply pending" pill when
    settings or acquisition timing are dirty;
  - always-visible bottom line: latest error, "All clear.", or a single
    coloured summary `N active warning(s) — expand for details.`;
  - a collapsible `Diagnostics` carries the previous full body
    (per-warning lines, dirty copy, notices).
- Analysis-extension and layer-card frames switched from
  `egui::Frame::group(ui.style())` to `theme::card_frame(ui)` so they
  pick up the design palette automatically. The layer / extension
  sections now use `theme::section_subhead` for the headings.

### `augur-gui/src/preview.rs` and `augur-gui/src/preview_renderer.rs`

- Polarity preview swapped from red/blue to magenta/cyan in all three
  rendering paths: the CPU fallback, the texture-sample shader, and the
  storage-buffer compute-accumulation shader. The shader-reference test
  helper that mirrors the GPU output for unit tests was updated to match.
- The user-facing labels for the polarity mode were renamed from
  `Red-Blue Polarity` / `Red-blue polarity ramp` to `Polarity` and
  `Polarity (magenta/cyan)`.
- The CPU-fallback test that asserted "channel 0 only" / "channel 2
  only" was tightened to express the new tints (magenta lights R+B with
  R dominating G; cyan lights G+B with R dark).

### `augur-gui/src/plugin_settings_ui.rs`

- The status-entry sparkline now reads its background, frame and stroke
  colours from `theme::palette_for_visuals` and `theme::ROI_*`, so it
  looks correct in both light and dark themes (previously it hard-coded
  a near-black panel and a fluorescent stroke that never adjusted).

## Pass 2 — components and shell polish

### `theme.rs`

- `Tone` enum (`Neutral`, `Success`, `Info`, `Warn`, `Error`,
  `PolarityOn`, `PolarityOff`) and a `theme::chip(ui, text, tone)` helper
  that paints a small monospace pill with a tinted fill and matching
  text colour. Used for plugin-phase labels, the dirty/ready badge, the
  recording indicator, and the `Finished` replay state.
- `theme::metric_pill(ui, value, unit, tone)` for the viewer status
  footer's primary row — a tabular-numeral value plus a small caption
  unit.
- `theme::primary_button(text)` returning a pre-styled `egui::Button`
  with the ink fill and white text for the single primary action on a
  surface.

### `app.rs`

- The top toolbar now opens with an "AugurRS" wordmark in the design
  display weight and ink colour, followed by a vertical separator before
  the menu items. This mirrors the `MenuBar.brand` row in the design.
- `● REC` is now a `Tone::Error` chip, and the post-replay `Finished`
  label is a `Tone::Success` chip — both consistent with the design's
  status-pill aesthetic.
- `stage_dirty_badge` switched from a coloured small label to
  `theme::chip` (`Tone::Warn` for dirty, `Tone::Success` for ready).
- Plugin and built-in analysis cards render the phase as an
  `Tone::Info` chip next to the heading instead of an inline
  `Phase: …` line. The `Metrics: N datasets, M panel views` line was
  shortened to `N datasets · M panel views` to match the design's
  middle-dot separator style.

### `viewer_widget.rs`

- `draw_status_footer` now opens with a primary metric row rendered via
  `theme::metric_pill` (throughput Mev/s, data rate MB/s, elapsed) and a
  second row of `ON % / OFF %` pills tinted with the polarity-mode
  colours plus the total events/frame in tabular numerals. The
  hover-readout / ruler-measurement line still follows underneath. The
  expandable `Pipeline details` body is unchanged.

## Pass 3 — plugin view chips, severity icons, layer counts

### `host_views.rs`

- New helper `ResolvedHostViewRegistry::window_views_for_provider(provider)`
  mirrors `panel_views_for_provider` but yields the
  `HostViewPlacement::Window` views attributed to a single plugin.

### `app.rs`

- `render_provider_view_chips(ui, provider)` paints a wrapped row of
  one-click chips for every window-placement view a plugin exposes.
  Each chip carries a phosphor icon (table / scatter / density / image /
  line) plus the view title. Clicking toggles the corresponding entry in
  `host_view_window_open`, which is the same map the View menu uses, so
  the deferred-viewport machinery picks it up automatically. Active
  chips render with the design's ink fill so an open view stays visible.
- New `host_view_kind_icon` helper centralises the `HostViewKind →
  phosphor glyph` mapping (used by the chips today, available for the
  future tabbed dock as well).
- The diagnostics warning rows in `Status & warnings` now lead with a
  matching phosphor severity glyph (`INFO`, `WARNING`,
  `WARNING_OCTAGON`) so users can scan severity without parsing colour
  alone.
- `Point cloud` and `Analysis layers` subheads gain a neutral
  `theme::chip` that shows the visible-count vs total-count
  (`2/2`, `1/3`), matching the right-of-subhead counter in the design.

## Pass 4 — multi-view host dock

### `app.rs` — dock state

`CameraApp` gained four fields that drive a tabbed dock at the bottom
of the central viewport:

- `dock_tabs: Vec<String>` — host-view IDs in tab order.
- `dock_active: Option<String>` — currently visible tab.
- `dock_open: bool` — collapsed state (the dock can be hidden without
  losing its tabs).
- `dock_height: f32` — drag-resizable height in points (clamped to
  `[160, 800]`, default 280).
- `dock_height_request: Option<f32>` — one-frame height override. egui
  owns the panel height through its `PanelState` once the panel has been
  shown, so maximize/restore has to push the new height through the
  panel's height range.
- `dock_defaults_seeded: bool` — the default tab set is seeded once. Any
  analysis parameter change re-resolves the host-view registry, and
  re-seeding there would reopen views the user closed.

### `app.rs` — dock helpers

- `dock_open_view(id)` adds the view if missing and focuses it; it also
  flips `dock_open` back to true so the dock un-collapses.
- `dock_close_view(id)` removes a tab and falls back to the previous
  tab when the active one closes.
- `dock_contains(id)` is the predicate used by view chips to decide
  whether to highlight as "in dock".

### `app.rs` — `render_host_view_dock(ctx)`

Renders an `egui::TopBottomPanel::bottom("host_view_dock")` *before* the
central panel so the central viewer keeps the remaining height. The
panel is only mounted when `dock_tabs` is non-empty. When `dock_open`
is false a thin collapsed strip with a single "expand" button replaces
the body, mirroring the design's `.dock-collapsed` row.

The body dispatches to the existing `render_host_view_content`. Because
that function already handles every kind (table, density, image,
scatter, line series), the dock works for every host-view kind a plugin
can publish without any per-kind rewrites.

Tab IDs are *not* pruned: `dock_tabs` records what the user wants
docked, and the registry can be empty for a few frames (epoch bump,
plugin reload). Each frame the dock renders only the ids that currently
resolve to a dockable view, falls back to the first renderable tab while
the active view is away, and mounts nothing at all when none resolve.

The panel also clips its contents to itself (`clip_to_panel`) — egui
hands panels a screen-wide clip rect, so anything that overflows would
otherwise paint over the side panels.

### `app.rs` — `render_dock_tab_strip`

Each tab is a small `egui::Frame` with the design's top-rounded
corners, with:

- a phosphor icon for the view kind (table / scatter / density / image /
  line),
- the view title (active fg = `palette.fg_0`, inactive = `palette.fg_2`),
- a neutral `theme::chip` carrying the kind tag (`table`, `scatter 2d`,
  `density`, …),
- an `X` close button.

The tabs live in a horizontal `ScrollArea` whose width always leaves
`DOCK_CONTROLS_WIDTH` for the action cluster, so more tabs than fit
scroll instead of pushing the controls out of the panel.

A right-aligned action cluster carries:

- `ARROW_SQUARE_OUT` to pop the active tab out into its existing
  deferred-viewport OS window (this clears the tab from the dock and
  flips the corresponding `host_view_window_open` entry to `true`).
- `ARROWS_OUT` / `ARROWS_IN` to maximize the dock and restore the height
  it had before.
- `CARET_DOWN` to collapse the dock body.

### `app.rs` — view-chip behaviour

The plugin-card view chips now route to the dock instead of toggling
the OS window directly:

- Click → `dock_open_view(id)` (add to dock or focus the existing tab).
- Right-click → toggle `host_view_window_open` (still available for
  users who want a free-floating OS window).

A chip styles itself as active when the view is in the dock *or* in a
window, so users always see "this view is open somewhere".

### `host_view_kind_tag`

Companion to `host_view_kind_icon` from pass 3 — returns the short
uppercase label that the dock tab strip's neutral chip displays.

## Verifying

```bash
cargo fmt --all -- --check
cargo clippy -p augur-gui --no-deps --tests -- -D warnings
cargo test  -p augur-gui --bins
```

All three pass. 98 tests green; no new tests were added because the
dock is purely view code reachable through the existing render
pipeline.

## What stayed

- ImageJ LUTs (Fire, Ice, Grays, Red Hot, Cyan Hot, Magenta Hot,
  Blue-White-Red) are scientific data colours — untouched.
- Raw event ON/OFF rendering colours (warm amber `#FFBA5C`, cool teal
  `#5CD6C9`) — untouched. They are the visual identity of the data.
- Annotation/ROI colour pickers, plot axes, `egui_plot` defaults — the
  design system explicitly says these must read as neutral instrument
  output and we did not theme them.
- The accent colour stays the documented `#1E64DC`. The chat-2 "Augury"
  twilight indigo-violet alternative was discussed but not adopted in
  this pass.

## Verifying

```bash
cargo fmt --all -- --check
cargo clippy -p augur-gui --no-deps -- -D warnings
cargo test  -p augur-gui --bins
```

All three pass. The `frame_to_color_image_renders_cpu_preview_without_overlay_compositing`
and `shader_reference_matches_cpu_reference_for_small_frame_modes` tests
verify the new polarity tints behave consistently across CPU and GPU
paths.

## Pass 5 — full audit closure

Audited every surface in the design bundle (menubar / settings panel /
viewer head / toolbar / display strip / canvas / transport / status
footer / dock / analysis panel / plugin card / components /
typography) and closed the remaining gaps:

### Brand & menubar

- **Owl logo** is loaded once via `load_brand_logo(ctx)` from
  `assets/logo.png` and rendered at 22 px before the wordmark.
- **Mode status pill** on the right side now always shows
  `● Idle / Previewing / Recording / Replaying` tinted by mode (`Tone`
  per state). The REC chip pulses by alternating `● REC` / `○ REC`
  every ~600 ms via `ctx.input(|i| i.time)` and a 120 ms repaint
  timer.
- **Layout pills** (`2D / Split / 3D`) replaced loose
  `selectable_value` calls with the new `theme::pill_cluster` —
  a single connected segmented control with ink-on/off semantics.
- **camera_status** label is rendered in `palette.fg_2` at 11 px.

### Side panels

- Both panels now open with a `theme::panel_header(ui, glyph, text)`:
  `CAMERA SETTINGS` (sliders icon) on the left, `ANALYSIS` (stack
  icon) on the right.
- Right panel **Notices** subsection is now separate, always-visible,
  above the existing Status & warnings block. Each notice carries a
  phosphor severity glyph (`INFO` / `WARNING` / `WARNING_OCTAGON`) and
  a row count chip beside the subhead.
- Right panel **Workspace** inspector switched from `ui.small()` lines
  to the design's two-column `theme::inspector_row(label, value)`
  layout. The shortcut hints below render as `theme::kbd_badge` pills
  ("1/2/3", "L", "Esc", "F") followed by a description.
- Right panel **Diagnostics** body is now a 2-column `egui::Grid`
  matching the design's `.diag-row` (label `palette.fg_3` / value
  `palette.fg_1` mono).

### Viewer head & canvas

- New **viewer head** strip above the toolbar: bold preview heading
  + monospace bus / serial / firmware meta strip. Mirrors
  `.viewer-head`.
- New **preview readout overlay** anchored to the canvas bottom-left
  shows `x · y / ON / OFF / total` in a translucent
  `Color32::from_black_alpha(180)` glass-morphism box, mono 11 px.
  Replaces the inline hover strip in the status footer.

### Transport

- **Step back** / **step forward** buttons added either side of
  Play/Pause. They route to the existing `replay_step_frames`
  output and are gated to `replay.paused` for step-back and
  `!replay.finished` for step-forward.
- **Custom scrub track** replaces egui's stock `Slider` — 4 px
  ink-filled bar with a circular thumb (6 px radius, ink stroke,
  `bg_1` fill). Drag-to-scrub and click-to-seek both work via
  `interact_pointer_pos`.

### 3D inspection toolbar

- Orientation pills (`ISO / XY / XZ / YZ`) now render through
  `theme::pill_cluster` for the same connected segmented look used
  in the menubar.

### Multi-view dock

- Persistence: `dock_height` and `dock_open` save into
  `eframe::Storage` so layout survives restart.
- Tab strip now shows the **plugin tag** (`· {plugin_name}`) after
  the title, plus a **maximize** button (`ARROWS_OUT`) that bumps
  the dock to ~70 % of viewport height. Each tab tooltip shows
  `kind · dataset_id`.
- Dock-collapsed strip lists tab names: `N host views docked ·
  title · title · …`, exactly matching the design's
  `.dock-collapsed` row.
- New `provider_plugin_tag(provider)` helper resolves
  `HostViewProviderKey::Runtime(idx)` to the loaded plugin name.

### Plugin card

- Disabled plugins now render a dimmed compact card (`ui.set_opacity(0.72)`)
  with the plugin name + `off` chip + a hint to enable from the
  Analysis menu — matches the design's `.is-off` style. Previously
  disabled plugins were skipped entirely.

### Components

- New `theme::panel_header`, `theme::kbd_badge`, `theme::inspector_row`,
  `theme::pill_cluster`, `theme::field_label` helpers complete the
  design's component primitives.
- `theme::primary_button` is now used by the Camera menu's `Apply
  Settings` action (ink fill, white text).
- `theme::field_label` is used by `plugin_settings_ui::render_setting_item`
  for `F64Drag` and `I64Drag` fields, giving them the design's
  `<label> [unit]` mono caption layout.

## Verifying

```bash
cargo fmt --all -- --check
cargo clippy -p augur-gui --no-deps --tests -- -D warnings
cargo test  -p augur-gui --bins
```

All three pass. 98 tests green; no new tests were added because the
new chrome is purely view code and reachable through the existing
render pipeline.

## Pass 6 — analysis-panel slimming and unified footer diagnostics

A side-by-side comparison against the reference design (`augurrs-design-system`
bundle, `ui_kits/workbench/index.html`) showed that the right `Analysis`
panel had grown busier than the reference and that the footer carried
the diagnostics line on three stacked rows instead of one. Pass 6
closes those gaps with no new tokens.

### `app.rs` — Workspace, Layers, Status & warnings

- The `Workspace` collapse opens **default-closed** instead of
  default-open. The link-ROI checkbox, layout/linked-ROI inspector
  rows and the keyboard-shortcut chips are still reachable through the
  collapse — they just no longer dominate the panel when there is no
  active selection.
- The `Layers` collapse was flattened. The two sub-headed groups
  (`Point cloud` and `Analysis layers`) merge into a single ordered
  layer list (raw events first, then plugin-published analysis layers).
  The visible/total counter (`2 / 5` style) moved up onto the
  `Layers ▾` header row using `crate::theme::collapse(.., right)`.
  The verbose explanatory paragraphs ("Raw event ON/OFF layers
  style…", "Raw-event history controls now live in the 3D toolbar…")
  and the `Show all layers` button were removed — visibility per row
  is the eye toggle, layer-style/isolate operations live in the
  per-row overflow menu.
- The right-panel `Status & warnings` block (subhead, mode line,
  warning summary, and the inline `Diagnostics` collapse with its
  `egui::Grid` of host-sync / active-warning / last-error rows) was
  removed. Its information is now surfaced in two places that suit
  the workflow better:
  - The **mode + dirty pending state** is shown by the menubar status
    chip's hover tooltip (`● Idle\nApply pending: settings, acq timing`).
  - The **per-warning notices** continue to live in the always-visible
    `Notices` section directly above where Status & warnings used to
    be.
  - **Pipeline / runtime diagnostics** (host sync, queue high-watermarks,
    preview-thread averages, etc.) live in the new viewer-footer
    `Diagnostics ▾` collapse — see below.
- The dead `draw_diagnostic_row` helper was removed.

### `app.rs` — menubar status

- The inline `camera_status` label that sat between the layout pill
  cluster and the mode chip was removed. The detailed
  `Camera not probed yet.` / `Replay finished.` / `Recording in
  progress.` / serial-firmware lines now surface via the mode chip's
  hover tooltip, together with the dirty-pending hint when applicable.
  The menubar reads as a single uncluttered row: brand, menus, layout
  cluster, status chip.

### `app.rs` — Extensions header

- `section_subhead` text changed from `Analysis extensions` to
  `Extensions` and is paired with a small `LIGHTNING` phosphor glyph,
  matching the design's `EXTENSIONS ⚡︎` head row. The
  "Only enabled analysis modules appear here…" caption was dropped.

### `viewer_widget.rs` — viewer head

- The viewer-head meta strip now reads
  `<bus> · <width × height> · firmware <ver>` when a probed camera is
  present, sourced from `DeviceInfo` and the active `CameraConfig` ROI
  resolution. Right-aligned on the same row, a permanent caption
  reads `Hover preview for pixel values · Esc clears tool` (the same
  hint that previously lived inside the status footer's hover row).

### `viewer_widget.rs` — unified diagnostics footer

- `draw_unified_diagnostics_row` replaces the previous
  `draw_status_metric_row` + `draw_polarity_split_row` stack with a
  single `·`-separated wrapping line:
  `24.8 Mev/s · 9.1 MB/s · 00:01:23 elapsed · ON 54.3% OFF 45.7% · 1,192,227 ev/frame`.
  The polarity ON / OFF percentages tint with the same magenta / cyan
  pair `Tone::PolarityOn` / `PolarityOff` already use elsewhere.
- `format_thousands` is a tiny helper that formats the per-frame
  event count with `,` group separators (`1,192,227` matches the
  design exactly).
- The `Pipeline details ▾` collapse renamed to `Diagnostics ▾`,
  matching the new copy. Its body still carries the existing
  pipeline-stat detail rows, the apply-pending warning, the
  per-warning analysis notices, the replay-open spinner and the last
  error — i.e. everything that used to live in the right-panel
  `Status & warnings` block now opens here from a single chevron
  attached to the row above the dock.
- `draw_status_metric_row` and `draw_polarity_split_row` are kept and
  marked `#[allow(dead_code)]` so any future surface that wants the
  old two-row layout (e.g. the headless CLI's TUI plans) can still
  reach them without re-deriving the formatting.

### Verifying

```bash
cargo fmt --all -- --check
cargo clippy --workspace --no-deps --tests -- -D warnings
cargo test  -p augur-gui --bins
```

All three pass. 98 tests green; no new tests were added because the
changes are layout-only and reachable through the existing render
pipeline.

## Pass 6.1 — chrome corrections

The first cut of Pass 6 left three visible-render bugs that did not
match the reference:

1. The `panel_header` painted a `bg_2` fill that, combined with its
   own inner margin, looked like a floating filled card with the
   collapse arrow stranded in empty space to the right. In the
   reference the header is flat — just the icon + uppercase title —
   with a single hairline separating it from the panel body.
2. The mode chip in the menubar always rendered `● <label>`. The
   reference shows a bare `●` while the camera is idle (the dot is
   enough chrome) and only adds a label when the user is actually
   doing something (`● Previewing`, `● REC`, `● Finished`).
3. The side-panel collapse arrow was `egui::Button::new("◀").frame(false)`,
   which renders as a bare glyph that floated out of the header row.
   The reference uses a small framed icon-button with a hover state.

### `theme.rs`

- `panel_header(ui, glyph, text, toggle)` is now flat: no fill, single
  bottom hairline, full-width row. The signature gained a
  `Option<PanelToggle>` so the trailing collapse button is part of the
  same row instead of being painted by the caller in a separate
  `with_layout` block. Returns `bool` — `true` when the toggle was
  clicked. Both side panels switched to the new signature.
- `PanelToggle { glyph, tooltip }` carries the phosphor glyph the
  caller wants on the right (`CARET_LEFT` for the left settings
  panel, `CARET_RIGHT` for the right analysis panel) and an optional
  tooltip string.
- `icon_button(ui, glyph, tooltip)` is the small framed icon-button
  primitive used by the panel-header toggle. Hover reveals a `bg_2`
  background and brightens the glyph from `fg_2` to `fg_0`; press
  goes one step darker (`bg_3`).

### `app.rs`

- The Idle status chip renders as `●` only — no "Idle" label. Other
  modes keep the label (`● Previewing`, `● Replaying`, `● Finished`,
  pulsed `● REC` / `○ REC`). The detailed `camera_status` text and
  the dirty-pending hint still surface via the chip's tooltip.
- Both `SidePanel::left("settings")` and `SidePanel::right("analysis")`
  now mount the panel header through the new `panel_header(.., toggle:
  Some(PanelToggle { … }))` form. The standalone right-aligned
  `▶`/`◀` buttons and their `with_layout(right_to_left)` blocks were
  removed.

### `viewer_widget.rs`

- The viewer head row now uses `allocate_ui_with_layout` to claim the
  full panel width up front. The right-aligned
  `Hover preview for pixel values · Esc clears tool` caption truly
  anchors to the right edge regardless of the heading's measured
  size — previously a narrow viewport caused the inner
  `right_to_left` layout to collapse against the heading.

## Pass 6.2 — right-panel layout bug fixes

User feedback after Pass 6.1 surfaced concrete rendering bugs in the
analysis panel:

1. The right `SidePanel` width changed when the `Layers` collapse
   opened. Cause: layer rows used a nested `right_to_left { count }`
   inside an outer `ui.horizontal { layer_row + right_to_left { dots } }`,
   producing a content-driven intrinsic-width request that the panel
   honoured. The `ScrollArea` was also configured with
   `auto_shrink([true, false])`, so when content was small the panel
   shrank horizontally too.
2. The Hotpixel extension card title used `ui.heading("Hotpixel
   Detection")` (~20 px), an `Info` chip with `host frame analysis`
   text, and a separate dirty pill — none of which appear in the
   reference. The reference uses a compact `eye + plugin-name + ●
   active` row.
3. The `Workspace` collapse defaulted to closed and showed the wrong
   content when opened (`Layout`, `Linked ROI`, `Clear selection`
   button, kbd-shortcut strip) — the design wants exactly three
   things: the Link checkbox, the `Selected` row, and the `Hover`
   row.

### `theme.rs`

- `layer_row` now allocates the full row width up front via
  `allocate_ui_with_layout` so its trailing `right_to_left { count }`
  block anchors to the right edge without forcing the SidePanel to
  grow. The row is a fixed 22 px high.
- `kbd_badge` is now `#[allow(dead_code)]` — kept for future surfaces
  that want the keycap pill, but no longer mounted in the Workspace
  collapse.

### `app.rs` — analysis panel

- `SidePanel::right("analysis")` switched to `resizable(true)` with
  tighter bounds (`min 320`, `default 360`, `max 480`). The inner
  `ScrollArea` now uses `auto_shrink([false, false])` so the panel's
  horizontal extent stays stable across collapse open/close.
- `render_investigation_layer_cards` wraps each row in a
  `allocate_ui_with_layout(full_width, …)` strip and renders the
  layer style/isolate detail pop-out via the row's
  `response.context_menu()` (right-click) instead of a separate
  `…` button. This removes the second nested `right_to_left` that
  was the actual width-pusher.
- `Workspace` is default-open again. Its body matches the reference
  exactly: `[✓] Link 2D ROI → 3D & tables`, `Selected: row #N · …`,
  `Hover: …`. The `Layout` / `Linked ROI` inspector rows, the
  `Clear selection` small-button and the kbd-shortcut strip were
  removed.
- `Notices` upgraded from a flat subhead-with-count-chip to a
  `theme::collapse(.., "Notices", true, Some(count))` block matching
  the reference. Notice rows route through a new `notice_row`
  free-function helper that allocates full row width and wraps long
  message text inside the panel rather than pushing it wider.
- The Hotpixel extension card title is now a one-row layout:
  `EYE` glyph + `hotpixel-detection` (13 px strong, `ink`) + a
  right-aligned `● active` `Tone::Success` chip; the dirty pill
  appears beside the chip when the stage is dirty. The redundant
  `host frame analysis` info chip and the multi-row metric / separator
  framing were removed — `render_ui` continues to emit the
  parameter sliders below.
- Runtime plugin cards (active and disabled) follow the same compact
  one-row-title pattern. Active cards add the phase chip alongside
  the `● active` status chip; disabled cards show an `EYE_SLASH`
  glyph, dimmed name and an `off` neutral chip.

### Verifying

```bash
cargo fmt --all -- --check
cargo clippy --workspace --no-deps --tests -- -D warnings
cargo test  -p augur-gui --bins
```

All three pass. 98 tests green.

## Pass 6.3 — plugin host-view interaction repair

Follow-up screenshot review showed that runtime plugin host views were leaking into the wrong
surfaces:

- `Scatter3dFromTable` descriptors now stay out of the dock, View menu, plugin-card chips, and
  deferred OS windows. They remain active as main investigation 3D scene layers.
- Compact `AnalysisPanel` host views now render inside the owning runtime plugin card, with
  provider/view-scoped egui ids so repeated EVE cards do not collide.
- Host-view chips use shortened labels plus full-title tooltips, preventing the narrow Analysis
  panel from wrapping labels into vertical strips.
- Default dock seeding now chooses only dock-renderable host views, so candidate 3D layer
  descriptors no longer produce an immediate dock mismatch error.

## Pass 7 — wrapping, alignment, and one primary per surface

A screenshot review of the Idle / Split / photodiode-plugin state turned up
one real layout bug and a set of alignment and duplication problems.

### The vertical-text bug

Pass 6.3 shortened host-view chip labels to stop the Analysis panel "wrapping
labels into vertical strips". That treated the symptom. The cause is that the
Analysis panel sets `style.wrap = Some(true)` for its whole subtree, and egui
lays a `Button`'s label out against the width *left on the current line*: once
two chips fill a row, the third is offered ~14 px and breaks one character per
line instead of moving down. Shortening labels only changes how many chips it
takes to reach that state.

`theme::wrap_row(ui, …)` is the fix — `horizontal_wrapped` with text wrapping
turned off inside, so every widget reports its natural width and the wrapping
placer breaks between widgets rather than inside them. Adopted by the host-view
chip row (`app.rs`), the capture-card action row, the summary/table action rows
in `host_views.rs`, and the plugin `Enum` radio row.

**Use `theme::wrap_row` for any row of buttons or chips in a narrow panel.**
Rows of pure prose keep `horizontal_wrapped`, where text wrapping is wanted.

### New `theme.rs` helpers

| Helper | Purpose |
| --- | --- |
| `wrap_row` | wrapping row whose widgets wrap as whole units (above) |
| `button_width` / `button_row_width` | galley-measured width of a default button, or of a row of them |
| `centered_row` | allocate a row at a given width, so a `top_down(Align::Center)` parent actually centres it |
| `action_button(primary, text)` | ink fill only when this copy of the action is the primary one |

`centered_row` exists because `Ui::horizontal_centered` centres **vertically**
— the canvas empty state's `Probe Camera` / `Open Replay…` row was left-aligned
under a centred heading for exactly that reason. `button_row_width` measures
from the laid-out galley rather than the previous frame's `min_rect`, so the
row is centred on its first frame with no visible jump.

`inspector_row` now returns its row `Response`, so callers can hang hover text
off the whole row.

### One primary per surface

`Probe Camera` was ink-filled twice at once — in the capture card and again in
the canvas empty state. `App::canvas_cta_visible` mirrors the placeholder's own
condition, and the capture card passes `!canvas_cta_visible` to
`theme::action_button`: the canvas CTA owns the emphasis while it is on screen,
the card takes it back once a pipeline runs.

### Menu bar, panels, and chrome

- The menu bar carried **two** top-level `Analysis` menus — offline runs and
  live toggles. Merged into one, with the toggles under a `Live analysis`
  subhead.
- `1 2D   2 Split   3 3D` beside the viewer title duplicated the `2D | Split |
  3D` pill cluster in the menu bar. Removed; `pill_cluster` takes a positional
  `tooltips` slice and teaches the shortcut on the control itself.
- Dock tabs dropped the kind chip that duplicated the glyph beside it, and show
  their close button only on the active or hovered tab — in an always-reserved
  slot, so revealing it never reflows the strip.
- The recording-file field filled the capture card instead of a fixed 130 px.
- Plugin `Path` rows reserve the browse button first and ellipsise the path
  from the front (`…/protocols/example.csv`), keeping the file name readable.
- Host-view empty messages route through `host_views::empty_state`, centred in
  the space the data would occupy rather than stranded top-left in a tall dock.
- The 3D overlay tray gets a second row when the canvas is too narrow, instead
  of scrolling controls out of sight and clipping a reading mid-token. The
  stored tray width is now the *unclamped* natural width — clamping it to the
  canvas made a too-wide tray indistinguishable from one that fits.

### Panel footers are reserved, never appended

A `ScrollArea` with `auto_shrink([false, false])` takes every remaining pixel of
its parent, so anything written *after* it lands outside the visible region —
present in the accessibility tree, invisible on screen. The left settings
panel's `Apply Settings` button and `Lock settings while recording` checkbox
were unreachable at 1210x768 for exactly this reason.

**Rule:** claim panel chrome that must stay on screen *before* the scroll area,
with `egui::TopBottomPanel::bottom(...).show_inside(ui, …)`. The scroll area
then fills what is left instead of what it wants.

With the footer reliably visible, the capture card's duplicate `Apply Settings`
button was removed — it only existed to work around the missing footer, and two
of the same action in one panel violates one-primary-per-surface. The footer
button's tooltip now states *why* it is disabled (locked, or nothing to send)
instead of leaving a dead control unexplained.

### Never touch `ui` or `ctx` inside a `data_mut` / `memory_mut` closure

`Context::data_mut` and `memory_mut` hold the context's `RwLock` for the whole
closure, and `parking_lot` locks are **not** reentrant. Any `ui.*` or `ctx.*`
call that reads context state from inside one — `rect_contains_pointer`,
`input`, `fonts`, `style` — self-deadlocks the UI thread: the window stops
repainting with no panic and no log line.

**Rule:** compute the value first, then store it.

```rust
let hovered = ui.rect_contains_pointer(rect);          // read the lock, release
ui.ctx().data_mut(|d| d.insert_temp(id, hovered));     // then take it for writing
```

`app::store_hover_state` is the sanctioned form for the hover-probe case, and
`storing_hover_state_never_reenters_the_context_lock` guards it — the probe runs
on its own thread so a regression fails on a timeout instead of hanging the
suite forever.

### Read-only means legible, not inert

`ui.add_enabled_ui(false, …)` around a whole section disables its collapsing
header too, so a panel advertised as a "read-only reference" cannot be opened
to read. `settings::section` wraps only the section *body*, leaving the header
live. Replay and locked recordings therefore still expand every settings
section; only the controls are greyed out.

### Sensor readout

See [`absolute-setting-values.md`](absolute-setting-values.md) and
[`viewer-toolbar-and-status-layout.md`](viewer-toolbar-and-status-layout.md).

### Verifying

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All pass; 145 tests green in `augur-gui`.

## Follow-ups (still open)

- Settings panel collapse counters (`5 / 5`, `0 / 64`) — needs the
  Bias / DEM-mask state to thread their counts up to the
  `CollapsingHeader` head row; deferred because the bias UI is owned
  by `settings.rs` and changes to its API would ripple out further
  than the design pass intended.
- Phosphor migration of the in-app **painted** viewer-tool glyphs
  (`viewer_tools`). The chrome already uses Phosphor everywhere
  icons are rendered as text; only the procedurally-painted strokes
  remain.
- Optional alternate "Augury" indigo-violet brand accent (chat-2
  proposal — held until you ask).
