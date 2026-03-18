# Plugin API v0.2

## Summary

`augur-plugin-api` now exposes one flat `PluginVTable` through the single `augur_plugin_vtable`
entry symbol. The host no longer supports legacy ABI descriptors, reconstruction-specific
compatibility hooks, or manifest fallbacks. Runtime plugins recompile once against the cleaned-up
interface and then interact with the host through host views, an EventStore-backed history handle,
and per-frame plus persistent JSON context storage.

## What Changed

- `PluginVTable` now includes host-view callbacks directly.
- `Plugin::process_frame()` receives `EventStoreHandle`, giving plugins read-only access to the
  retained decoded event history.
- `HostContext` now has both per-frame `publish()` / `get()` and persistent
  `publish_persistent()` / `get_persistent()` helpers.
- `augur-gui` requires a valid `plugin.toml` before attempting to load a runtime plugin.
- `roi-grid` remains the only built-in analysis plugin in this repository; scientific runtime
  plugins now live in [augur-plugins](https://github.com/muthmann/augur-plugins).

## Operational Notes

- Event history retention is host-owned and memory-budgeted, with a default budget of `100 MiB`.
- The settings panel exposes the EventStore memory budget so users can trade retention depth
  against RAM use without recompiling.
- Any plugin built against the pre-v0.2 host-view / ABI-v2 transition layer must be rebuilt before
  it can load into this host.

## Verification

```bash
cargo fmt --all
cargo build
cargo test
rg "accumulated_localizations|PluginVTableV2|PluginAbiDescriptor|PLUGIN_ENTRY_V2|fallback" augur-plugin-api/src augur-gui/src
```
