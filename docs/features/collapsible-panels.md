# Collapsible GUI Panels

## Summary

The GUI settings panel and analysis panel can now be collapsed independently from arrow controls placed on the panel separator edges. Both side panels also scroll end to end, so long settings/plugin lists remain reachable without sacrificing preview space.

## Behavior

- the left settings panel collapses to a narrow 22 px edge strip with a frameless `▶` arrow
- the right analysis panel collapses to a narrow 22 px edge strip with a frameless `◀` arrow
- collapsed arrows use 14 px weak text color for a minimal, non-button appearance; hover highlights are still present
- each expanded panel shows its collapse arrow on the inner separator edge
- panel visibility can also be toggled from the `View` menu in the menu bar
- both panel bodies are wrapped in a vertical scroll area
- panel visibility does not reset plugin enablement or camera settings

When the analysis panel is hidden, plugins still run if they are enabled. Hiding the panel only affects layout.

## Layout Notes

- the preview area expands into the freed space immediately when either side panel is hidden
- the analysis panel still auto-suppresses itself when no plugins are enabled
- replay mode and live mode share the same collapse controls

## Files

| File | Role |
|---|---|
| `augur-gui/src/app.rs` | Edge toggle buttons, collapsed strips, and scrollable side-panel rendering |
| `docs/gui.md` | User-facing GUI workflow documentation |

## Verification

- open the GUI
- collapse and expand each panel from its separator-edge button
- confirm long settings/plugin content scrolls to the end
- confirm the preview resizes and no plugin/camera state is lost
