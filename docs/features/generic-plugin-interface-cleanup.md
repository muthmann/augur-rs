# Generic Plugin Interface Cleanup

## Summary

This change finishes the pre-v1 cleanup of the runtime plugin boundary:

- `augur-plugin-api` is now the generic host/runtime contract only
- domain payloads such as localization types moved into `augur-plugin-types`
- the reconstruction-specific runtime hook was removed
- host-owned plugin UI now flows only through the generic host-view registry
- retained event history is now an explicit plugin capability

## What Changed

- Added the new workspace crate `augur-plugin-types`
- Moved `Localization`, `LocalizationResults`, `LocalizationTable`, `LocalizationRow`, and
  `CTX_LOCALIZATION_RESULTS` into `augur_plugin_types::localization`
- Removed `accumulated_localizations` from the safe trait, flat runtime vtable, export macro, and
  runtime loader
- Added `PluginCapabilities { retained_event_history: bool }`
- Added generic `Image2dV1` and `Series1dV1` dataset kinds
- Added generic `Scatter2dFromTable`, `ImageWindow`, and `LineSeriesWindow` host views
- Removed the dedicated reconstruction window/state/export pipeline from `augur-gui`
- Stopped retaining empty decoded-event frames in the host `EventStore`

## Breaking Changes

This is an intentional pre-v1 break.

Runtime plugins must rebuild and update their code if they:

- imported localization payloads from `augur-plugin-api`
- implemented or consumed `accumulated_localizations`
- relied on retained event history being populated implicitly
- assumed the host only supported table/density host-view kinds

There is no compatibility shim for older plugins.

## Future-Oriented Direction

The plugin host now favors:

- machine-readable datasets over plugin-specific UI hooks
- host-owned rendering over plugin-owned `egui`
- explicit capabilities over implicit runtime cost
- optional companion crates for domain payloads instead of leaking domain semantics into the core
  API

That should make it easier to support future plugins across very different domains without
rewriting the host shell around each one.

## Verification

```bash
cargo test -p augur-plugin-api
cargo test -p augur-gui host_view
cargo test -p augur-gui event_store
cargo check -p augur-gui
```
