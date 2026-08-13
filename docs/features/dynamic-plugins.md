# Dynamic Plugin Loading

## Goal

Allow plugin authors to build a `cdylib`, copy it into the user plugin directory, and load or reload it in `augur-gui` without recompiling the host application.

## Plugin Directory

The default plugin directory is:

```text
~/.augur/plugins/
```

Each plugin gets its own subdirectory:

```text
~/.augur/plugins/
  example/
    plugin.toml
    libaugur_plugin_example.dylib   # macOS
```

Library extensions vary by platform:

- macOS: `.dylib`
- Linux: `.so`
- Windows: `.dll`

## Manifest

Each plugin directory must contain a `plugin.toml`:

```toml
id = "example.plugin"
name = "Example Plugin"
version = "0.2.0"
description = "Demonstrates the runtime plugin manifest format."
domain = "general"
library = "augur_plugin_example"
host_commands = [
  "start_recording",
  "stop_recording",
  "apply_camera_configuration",
  "restore_camera_configuration",
]
```

Fields:

| Field | Required | Description |
|---|---|---|
| `id` | for control plane | Stable, unique routing identity. Lowercase ASCII letters/digits plus `.`, `-`, `_`. |
| `name` | yes | Display name shown in the Plugin Manager |
| `version` | yes | Semantic version string |
| `description` | yes | One-line summary |
| `domain` | yes | Category tag (e.g. `general`, `analysis`, `vision`, `robotics`) |
| `library` | no | Base name of the dynamic library, **without** the `lib` prefix and without the platform extension (`.dylib`, `.so`, `.dll`). If omitted, the loader auto-discovers a single library file in the directory. |
| `host_commands` | no | Closed host-operation verbs the plugin may request: `start_recording`, `stop_recording`, `apply_camera_configuration`, and `restore_camera_configuration`. Omitted means no host-command capability. |

For example, `library = "augur_plugin_example"` resolves to `libaugur_plugin_example.dylib` on macOS, `libaugur_plugin_example.so` on Linux, and `augur_plugin_example.dll` on Windows.

## Plugin Manager Workflow

The GUI exposes a dedicated Plugin Manager window:

- **Show Plugin Manager** opens the table of discovered plugins
- **Scan for New Plugins** rescans the plugin directory
- **Reload** unloads and reloads one plugin instance
- **Open Plugins Folder** opens `~/.augur/plugins/` in the OS file browser

Load failures are recorded per plugin instead of aborting the app.

They are **also** surfaced in the analysis panel's `Status & warnings` section, one row per failed
plugin (`<name> · not loaded: <error>`), and counted in that section's badge. A plugin that never
loaded is a capability the session does not have, so "All clear" must not be able to hide one — see
ADR 033.

The loader validates both the exported `PluginVTable` size and the explicit ABI version before it
calls any plugin function pointers. A stale plugin now shows up as a normal load error in the
Plugin Manager instead of crashing AugurRS during startup.

The loader and FFI callback bridges live in `augur-runtime`. `augur-gui` keeps a
GUI-side plugin manager for settings and status, while live analysis uses a
worker-owned plugin manager loaded from the same plugin directory.

## Runtime Plugin Sources

The companion [augur-plugins](https://github.com/muthmann/augur-plugins) repository hosts the plugin template, authoring docs, and community plugin implementations. Build a plugin there (or start from the template), then copy its `plugin.toml` plus release library into `~/.augur/plugins/<plugin-name>/`.

## Troubleshooting

### "missing field `name`"

The `plugin.toml` uses the deprecated `[plugin]` section format:

```toml
[plugin]
name = "..."
```

Update it to top-level fields as shown in the Manifest section above.

### "no .dylib / .so / .dll found"

A source folder was copied instead of the built library. Build in `--release` mode first, then copy the generated dynamic library.

### "loading symbol augur_plugin_vtable failed"

The library was built against a different version of the plugin API. Rebuild it against the current `augur-plugin-api` release before loading it into this host.

### "plugin ABI mismatch"

The plugin was compiled against an older `augur-plugin-api` layout whose vtable size may still look
compatible by coincidence. Rebuild the plugin against the current host release, then replace the
old library in `~/.augur/plugins/<plugin-name>/`.

The current host expects ABI v6. It adds explicit runtime roles and the
frame-independent semantic service/host-command control plane. Rebuild every
runtime plugin after upgrading the host.

## Why C FFI + `libloading`

The plugin surface is intentionally small:

- frame input
- overlay/warning output
- settings schema
- status entries
- context publish/get

That made a hand-rolled C ABI simpler than a heavier cross-library abstraction layer while still keeping the boundary explicit and testable.
