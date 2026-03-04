# Getting Started

## Requirements

- macOS
- Rust toolchain with Cargo installed
- Prophesee EVK4 with IMX636 sensor
- Direct USB 3.0 connection to the host machine

## Build

```bash
cargo build --workspace
```

Optional local checks:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Confirm The Camera Is Reachable

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
- prefer a direct cable over hubs or adapters when possible
- close other applications that may already own the device
- rebuild once after dependency changes so `rusb` and the binaries are up to date

If you observe better throughput or stability with a direct USB-C cable than with a USB-A adapter path, treat that as a real hardware-path variable.

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

Launch the desktop app:

```bash
cargo run --bin augur-gui
```

Suggested first run:

1. Click `Probe` to detect the EVK4 and populate device information.
2. Click `Preview` to start live display.
3. Adjust ROI, pixel mask, or digital filters in the left settings panel.
4. Click `Apply Settings` to push runtime changes while previewing.
5. Click `Record` when you are ready to capture to disk.

## Next Steps

- Continue with [Configuration Reference](configuration.md)
- Use [CLI Usage](cli.md) for headless workflows
- Use [GUI Usage](gui.md) for preview and analysis flows
- Read [Performance Notes](performance.md) before making benchmark claims
