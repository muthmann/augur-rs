# Collapsible GUI Panels

## Summary

The GUI settings panel and analysis panel can now be collapsed independently from the toolbar. This lets users reclaim horizontal space for the preview without disabling plugins or losing their current configuration state.

## Behavior

- `Settings Panel` toggles the left-side camera settings panel
- `Analysis Panel` toggles the right-side plugin panel
- both toggles are available in the top toolbar
- panel visibility does not reset plugin enablement or camera settings

When the analysis panel is hidden, plugins still run if they are enabled. Hiding the panel only affects layout.

## Layout Notes

- the preview area expands into the freed space immediately when either side panel is hidden
- the analysis panel still auto-suppresses itself when no plugins are enabled
- replay mode and live mode share the same collapse controls

## Files

| File | Role |
|---|---|
| `augur-gui/src/app.rs` | Toolbar toggle buttons and conditional side-panel rendering |
| `docs/gui.md` | User-facing GUI workflow documentation |
| `book/src/gui.md` | mdBook GUI guide |

## Verification

- open the GUI
- toggle each panel independently
- confirm the preview resizes and no plugin/camera state is lost
