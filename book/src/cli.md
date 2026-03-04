# CLI Usage

The `augur` binary provides a direct command-line workflow for probing hardware, recording data, and editing configuration files.

## Command Summary

```bash
cargo run --bin augur -- --help
```

Available commands:

- `status`
- `record`
- `config show`
- `config set-bias`
- `config set-roi`
- `config set-mask`

## `status`

Show the detected EVK4 device information and the effective camera settings.

```bash
cargo run --bin augur -- status
cargo run --bin augur -- status --config augur.toml
```

## `record`

Record an EVT3 `.raw` stream to disk.

```bash
cargo run --bin augur -- record captures/session.raw
cargo run --bin augur -- record captures/session.raw --config augur.toml --duration-s 30
```

During recording the CLI prints current throughput and total written data once per second.

## `config show`

Print the effective configuration as TOML.

```bash
cargo run --bin augur -- config show
cargo run --bin augur -- config show --config augur.toml
```

## `config set-bias`

Update one bias key in a TOML file.

```bash
cargo run --bin augur -- config set-bias diff_on 10
cargo run --bin augur -- config set-bias refr 40 --config profiles/fast.toml
```

## `config set-roi`

Update the ROI rectangle in a TOML file.

```bash
cargo run --bin augur -- config set-roi 100 50 640 360
```

## `config set-mask`

Append one masked pixel to a TOML file.

```bash
cargo run --bin augur -- config set-mask 512 288
```

## Notes

- The mutating `config` subcommands default to `augur.toml` in the current working directory
- `status` and `record` can run without a config file and fall back to built-in defaults
- Press `Ctrl+C` during `record` to stop cleanly
