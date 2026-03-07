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

Each plugin directory must contain a `plugin.toml` with loader metadata such as:

- `name`
- `version`
- `description`
- `domain`
- optional `library` base name

If `library` is omitted, the loader falls back to auto-discovering a single matching dynamic library in the directory.

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

## Why C FFI + `libloading`

The plugin surface is intentionally small:

- frame input
- overlay/warning output
- settings schema
- status entries
- context publish/get

That made a hand-rolled C ABI simpler than a heavier cross-library abstraction layer while still keeping the boundary explicit and testable.
