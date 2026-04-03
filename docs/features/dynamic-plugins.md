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
name = "Example Plugin"
version = "0.2.0"
description = "Demonstrates the runtime plugin manifest format."
domain = "general"
library = "augur_plugin_example"
```

Fields:

| Field | Required | Description |
|---|---|---|
| `name` | yes | Display name shown in the Plugin Manager |
| `version` | yes | Semantic version string |
| `description` | yes | One-line summary |
| `domain` | yes | Category tag (`general`, `smlm`, `biophotonics`, …) |
| `library` | no | Base name of the dynamic library, **without** the `lib` prefix and without the platform extension (`.dylib`, `.so`, `.dll`). If omitted, the loader auto-discovers a single library file in the directory. |

For example, `library = "augur_plugin_example"` resolves to `libaugur_plugin_example.dylib` on macOS, `libaugur_plugin_example.so` on Linux, and `augur_plugin_example.dll` on Windows.

## Plugin Manager Workflow

The GUI exposes a dedicated Plugin Manager window:

- **Show Plugin Manager** opens the table of discovered plugins
- **Scan for New Plugins** rescans the plugin directory
- **Reload** unloads and reloads one plugin instance
- **Open Plugins Folder** opens `~/.augur/plugins/` in the OS file browser

Load failures are recorded per plugin instead of aborting the app.

## Runtime Plugin Sources

Maintained scientific runtime plugins now live in
[augur-plugins](https://github.com/muthmann/augur-plugins). Build a plugin
there, then copy its `plugin.toml` plus release library into
`~/.augur/plugins/<plugin-name>/`.

## Troubleshooting

### "missing field `name`"

The `plugin.toml` still uses the old `[plugin]` section format:

```toml
[plugin]
name = "..."
```

Update it to top-level fields as shown in the Manifest section above.

### "no .dylib / .so / .dll found"

A source folder was copied instead of the built library. Build in `--release` mode first, then copy the generated dynamic library.

### "loading symbol augur_plugin_vtable failed"

The library was built against an older runtime plugin API. Rebuild it against the current
`augur-plugin-api` branch or release before loading it into this host.

## Why C FFI + `libloading`

The plugin surface is intentionally small:

- frame input
- overlay/warning output
- settings schema
- status entries
- context publish/get

That made a hand-rolled C ABI simpler than a heavier cross-library abstraction layer while still keeping the boundary explicit and testable.
