# HDF5 File Support

## Summary

AugurRS can replay Prophesee HDF5 recordings (`.h5` / `.hdf5`) when `augur-gui` is built with the `hdf5` feature, but many real captures are compressed with Prophesee's ECF codec (HDF5 filter `0x8ECF`). Those files need both a system HDF5 installation and the Prophesee HDF5 ECF plugin available through `HDF5_PLUGIN_PATH`.

Without the plugin, HDF5 reads fail inside the native library with errors like `can't open directory (/usr/local/hdf5/lib/plugin)` or `failed to read CD/events`.

## Distribution Support

HDF5 replay is a build-from-source feature. Tagged binary releases do **not** include it.

| Channel | HDF5 available? | Why |
|---|---|---|
| GitHub release binaries / macOS app bundle | **No** | The release workflow builds `augur` and `AugurRS` without `--features hdf5`. Even if it did not, the resulting binaries would still depend on a separately installed `libhdf5` and the ECF plugin at runtime. |
| Build from source | **Yes** | Install system HDF5, install the ECF plugin, export `HDF5_DIR` and `HDF5_PLUGIN_PATH`, then build `augur-gui` with `--features hdf5`. |

## Build And Runtime Requirements

- Install native HDF5 development files first.
  - macOS: `brew install hdf5`
  - Ubuntu/Debian: `sudo apt install libhdf5-dev`
- On macOS with Homebrew, export `HDF5_DIR="$(brew --prefix hdf5)"` before any `cargo build` or `cargo run ... --features hdf5` command.
- ECF-compressed Prophesee files require the HDF5 ECF plugin at runtime via `HDF5_PLUGIN_PATH`.
- On macOS, keep `HDF5_PLUGIN_PATH` exported for HDF5-enabled build/test commands as well as when launching the GUI so the native HDF5 stack resolves the codec consistently.

## ECF Compression Plugin

The ECF plugin is published in [prophesee-ai/hdf5_ecf](https://github.com/prophesee-ai/hdf5_ecf). No prebuilt releases are provided there, so CameraSDK includes a helper script to clone, build, and install it locally.

### Automated Setup (recommended)

```bash
./scripts/install-ecf-plugin.sh
./scripts/install-ecf-plugin.sh --prefix /custom/path
./scripts/install-ecf-plugin.sh --force
```

The script:

- auto-detects HDF5 from `HDF5_DIR`, Homebrew, `pkg-config`, or `h5cc`
- builds the upstream plugin with CMake
- installs it to `~/.local/share/hdf5/plugin/` by default
- prints the `HDF5_PLUGIN_PATH` export line to add to your shell config

### Manual Setup

```bash
git clone https://github.com/prophesee-ai/hdf5_ecf.git
cmake -S hdf5_ecf -B hdf5_ecf/build -DCMAKE_BUILD_TYPE=Release \
  -DHDF5_ROOT="$(brew --prefix hdf5)"
cmake --build hdf5_ecf/build --parallel

export HDF5_PLUGIN_PATH="$PWD/hdf5_ecf/build/lib/hdf5/plugin"
```

If you prefer an installed location instead of using the build tree directly, run `cmake --install hdf5_ecf/build` and point `HDF5_PLUGIN_PATH` at that install destination.

## Environment Variables

```bash
export HDF5_DIR="$(brew --prefix hdf5)"
export HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin"
```

Use those exports before HDF5-enabled GUI commands:

```bash
cargo build -p augur-gui --bin AugurRS --features hdf5
cargo run -p augur-gui --bin AugurRS --features hdf5
```

If you do not want them globally, prefix the commands directly:

```bash
HDF5_DIR="$(brew --prefix hdf5)" \
HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin" \
cargo run -p augur-gui --bin AugurRS --features hdf5
```

## Verification

```bash
HDF5_PLUGIN_PATH="$HOME/.local/share/hdf5/plugin" \
HDF5_DIR="$(brew --prefix hdf5)" \
cargo build --release -p augur-gui --bin AugurRS --features hdf5
```

After that build succeeds, launch `AugurRS` with the same environment and open the target `.h5` / `.hdf5` replay file.
