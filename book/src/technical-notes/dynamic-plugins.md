# Dynamic Plugin Loading

The default plugin directory is `~/.augur/plugins/`.

Each plugin lives in its own subdirectory:

```text
~/.augur/plugins/
  hotpixel/
    plugin.toml
    libaugur_plugin_hotpixel.dylib   # .so on Linux, .dll on Windows
```

## plugin.toml

```toml
name = "Hotpixel Detection"
version = "0.1.0"
description = "Detects persistently noisy pixels."
domain = "general"
library = "augur_plugin_hotpixel"
```

`library` is the base name without the `lib` prefix and without the platform extension.

## Plugin Manager

The GUI Plugin Manager can:

- rescan the plugin directory
- reload one plugin without restarting the host
- open the plugin folder in the OS file browser
- surface load errors per plugin without crashing the app

## Reference Plugins

This workspace includes reference plugin crates under `plugins/` for hotpixel detection, molecule localization, and focus metrics. Community plugins live in [augur-plugins](https://github.com/muthmann/augur-plugins).

See [docs/features/dynamic-plugins.md](../../docs/features/dynamic-plugins.md) for the full install and troubleshooting guide.
