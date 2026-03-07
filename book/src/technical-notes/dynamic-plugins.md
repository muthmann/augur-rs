# Dynamic Plugin Loading

The default plugin directory is `~/.augur/plugins/`.

Each plugin lives in its own subdirectory with:

- a `plugin.toml`
- one platform-specific dynamic library (`.dylib`, `.so`, or `.dll`)

The GUI Plugin Manager can:

- rescan the plugin directory
- reload one plugin
- open the plugin folder in the OS file browser
- surface load errors without crashing the app

This workspace includes reference plugin crates under `plugins/` for hotpixel detection, molecule localization, and focus metrics.
