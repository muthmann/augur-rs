# Theme-Aware GUI Status Colors

## Summary

`augur-gui` now renders status, warning, and error text with colors that remain legible in both light and dark themes. Warning and error labels use egui's theme-aware foreground colors, while success and info labels use darker custom shades that preserve contrast on light backgrounds without disappearing on dark ones. The `View -> Dark Mode` toggle now gives the user an explicit appearance switch, and deferred viewer windows follow that same theme choice.

## Behavior

- replay notices use the active theme's warning color instead of a hardcoded yellow
- missing plugin dependency notices and plugin load errors follow the active theme's warning/error colors
- the Plugin Manager `Loaded`/`Error` status labels use a contrast-safe green/warning pairing
- the replay `Finished` label uses the same mid-green success color
- analysis warnings now derive warning/error colors from `egui::Visuals`
- analysis info notices use a darker blue for better readability in light mode
- the unapplied-runtime-changes warning and the last-error label also follow the active theme
- the `View -> Dark Mode` checkbox switches the root UI, popup viewer, host-view windows, histogram window, and line-profile window together

## Scope

This change affects host-owned GUI presentation in `augur-gui`. Overlay colors rendered on top of the preview image and plugin-specific chart colors are unchanged.

## Files

| File | Role |
|---|---|
| `augur-gui/src/app.rs` | Theme preference state, theme-aware warning/error text, contrast-safe success/info colors, and deferred viewport theme propagation |
| `augur-gui/src/viewer_tools/histogram.rs` | Histogram viewport theme propagation |
| `augur-gui/src/viewer_tools/line_profile.rs` | Line-profile viewport theme propagation |
| `docs/gui.md` | User-facing GUI guide |

## Verification

- `cargo fmt --all`
- `cargo build -p augur-gui`
- `cargo test -p augur-gui`
- manual light/dark theme check recommended for final visual confirmation
