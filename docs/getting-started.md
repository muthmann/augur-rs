# Getting Started

## Requirements

- Rust toolchain with Cargo installed
- macOS, Linux, or Windows

**For live camera capture only** (not needed for replay and analysis):
- Prophesee EVK4 with IMX636 sensor
- Direct USB 3.0 connection to the host machine

## Build

Binary releases do not include HDF5 replay support. Use a source build for `.h5` / `.hdf5` replay.

```bash
cargo build --workspace

# Optional: enable `.h5` / `.hdf5` replay in the GUI
# Requires a system HDF5 installation such as `brew install hdf5`.
export HDF5_DIR="$(brew --prefix hdf5)"   # macOS / Homebrew
./scripts/install-ecf-plugin.sh
export HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin"
cargo build -p augur-gui --bin AugurRS --features hdf5
```

`HDF5_PLUGIN_PATH` must be set before launching `AugurRS` for ECF-compressed Prophesee files and, on macOS, should also be present for HDF5-enabled build/test commands. See [HDF5 file support](./features/hdf5-file-support.md) for the manual plugin fallback and environment details.

Optional local checks:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Install AugurRS.app On macOS

For day-to-day use on macOS, prefer building the app bundle once and launching it from
Applications instead of rerunning `cargo run` every time.

```bash
./scripts/build-macos-app.sh --install
```

This builds `target/release/AugurRS`, assembles `dist/local-macos/AugurRS.app`, and copies the app
into `/Applications` by default.

Useful variants:

```bash
# Install without admin rights
./scripts/build-macos-app.sh --install --install-dir "$HOME/Applications"

# Create a local DMG for installer testing
./scripts/build-macos-app.sh --dmg
```

After that you can launch `AugurRS.app` from Finder, Spotlight, or the Dock like a normal desktop
application.

## Replay-Only Quick Start (No Camera Required)

You do not need a camera to use AugurRS. Open any supported event file and get the full
investigation workspace with 2D preview, GPU-accelerated 3D inspection, cross-view linked
selection, and plugin analysis.

```bash
cargo run -p augur-gui --bin AugurRS
```

1. Use `File -> Open Replay` to load a `.raw`, `.csv`, `.bin`, `.npy`, or `.h5` / `.hdf5` file.
2. Press `2` or use the toolbar to switch to split 2D+3D view.
3. Use the timeline to scrub through the recording.
4. Enable plugins from `Analysis` to run live analysis on the replayed data.
5. Click markers in 2D, rows in tables, or points in 3D — selections are linked across all views.
6. Press `L` to link the 2D ROI to the 3D view for spatial filtering.

If you have a camera and want to do live capture, continue below.

---

## Confirm the Camera Is Reachable

Run:

```bash
cargo run --bin augur -- status
```

Expected result:

- device model, vendor, and optionally serial and firmware
- current ROI
- number of masked pixels
- current digital filter state

If the command fails to open the EVK4:

- confirm the camera is powered and connected directly over USB 3.0
- use a direct cable — hubs and adapters can limit throughput
- close other applications that may already own the device
- rebuild after dependency changes so `rusb` and the binaries are up to date

## First Recording

Record with defaults:

```bash
cargo run --bin augur -- record captures/first.raw --duration-s 10
```

This writes:

- `captures/first.raw`
- `captures/first.toml`

The `.toml` file stores the effective camera configuration used for the capture.

## First GUI Session

Launch the desktop app from Applications if you used the install flow above. For active development
or quick testing, you can also run it directly from Cargo:

```bash
cargo run -p augur-gui --bin AugurRS
```

Suggested first run:

1. Click `Probe` to detect the EVK4 and populate device information.
2. Click `Preview` to start live display.
3. Press `2` to switch to split 2D+3D view and see live events in both preview and 3D point cloud.
4. Adjust pixel scale, acquisition time, or retained history budget from the top-bar `Settings` menu.
5. Adjust ROI, pixel mask, or digital filters in the left settings panel.
6. Click `Apply Settings` to push live camera changes while previewing.
7. Click `Record` when you are ready to capture to disk.

## Next Steps

- Use [GUI usage](./gui.md) for the investigation workspace, cross-view analysis, and plugin workflows
- Use [Investigation Workspace](./features/investigation-workspace.md) for details on linked 2D/3D views, selection, and layer styling
- Use [Configuration reference](./configuration.md) to create a reusable TOML file
- Use [CLI usage](./cli.md) for headless operation and scripting
- Use [HDF5 file support](./features/hdf5-file-support.md) when replaying `.h5` / `.hdf5` files
- Read [Performance](./performance.md) for architecture and design rationale
