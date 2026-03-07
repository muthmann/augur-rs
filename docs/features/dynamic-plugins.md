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
  hotpixel/
    plugin.toml
    libaugur_plugin_hotpixel.dylib   # macOS
```

Library extensions vary by platform:

- macOS: `.dylib`
- Linux: `.so`
- Windows: `.dll`

## Manifest

Each plugin directory must contain a `plugin.toml`:

```toml
name = "Hotpixel Detection"
version = "0.1.0"
description = "Detects persistently noisy pixels."
domain = "general"
library = "augur_plugin_hotpixel"
```

Fields:

| Field | Required | Description |
|---|---|---|
| `name` | yes | Display name shown in the Plugin Manager |
| `version` | yes | Semantic version string |
| `description` | yes | One-line summary |
| `domain` | yes | Category tag (`general`, `smlm`, `biophotonics`, …) |
| `library` | no | Base name of the dynamic library, **without** the `lib` prefix and without the platform extension (`.dylib`, `.so`, `.dll`). If omitted, the loader auto-discovers a single library file in the directory. |

For example, `library = "augur_plugin_hotpixel"` resolves to `libaugur_plugin_hotpixel.dylib` on macOS, `libaugur_plugin_hotpixel.so` on Linux, and `augur_plugin_hotpixel.dll` on Windows.

## Plugin Manager Workflow

The GUI exposes a dedicated Plugin Manager window:

- **Show Plugin Manager** opens the table of discovered plugins
- **Scan for New Plugins** rescans the plugin directory
- **Reload** unloads and reloads one plugin instance
- **Open Plugins Folder** opens `~/.augur/plugins/` in the OS file browser

Load failures are recorded per plugin instead of aborting the app.

## Reference Plugins in This Workspace

This repository currently includes reference plugin crates that exercise the runtime:

- `plugins/hotpixel`
- `plugins/localization`
- `plugins/focus-metrics`

Example build flow:

```bash
cargo build -p augur-plugin-hotpixel --release
mkdir -p ~/.augur/plugins/hotpixel
cp plugins/hotpixel/plugin.toml ~/.augur/plugins/hotpixel/
cp target/release/libaugur_plugin_hotpixel.dylib ~/.augur/plugins/hotpixel/
```

After copying the files, launch `augur-gui` and use **Plugins → Scan for New Plugins**.

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

The library was built against the old compile-time plugin API. Port it to `augur-plugin-api::Plugin` and export it with `export_plugin!`.

## Why C FFI + `libloading`

The plugin surface is intentionally small:

- frame input
- overlay/warning output
- settings schema
- status entries
- context publish/get

That made a hand-rolled C ABI simpler than a heavier cross-library abstraction layer while still keeping the boundary explicit and testable.
