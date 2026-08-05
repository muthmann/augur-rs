# CLI Usage

The `augur` binary provides a direct command-line workflow for probing hardware, recording data, and editing configuration files.

## Command Summary

```bash
cargo run --bin augur -- --help
```

Available commands:

- `status`
- `record`
- `analyze`
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
cargo run --bin augur -- record captures/session.raw --experiment-id exp-42 --operator "Ada Lovelace"
```

During recording the CLI prints current throughput and total written data once per second.

Optional metadata flags:

- `--experiment-id` stores a lab or notebook reference in the sidecar metadata
- `--operator` stores who ran the recording in the sidecar metadata
- `--notes` stores free-form recording notes in the sidecar metadata

## `analyze`

Run deterministic whole-file plugin analysis and export host-view datasets.

```bash
cargo run --bin augur -- analyze captures/session.raw --out analysis/session
cargo run --bin augur -- analyze captures/session.raw --config analysis.toml --out analysis/session
cargo run --bin augur -- analyze captures/session.raw --plugins-dir ~/.augur/plugins --out analysis/session
```

The output directory must not already exist. The runner writes into a temporary
directory and renames it only after the analysis and exports complete.

Example config:

```toml
t_start_us = 0
acq_time_ms = 1

[plugins."Example Plugin"]
enabled = true

[plugins."Example Plugin".settings]
threshold = 12.5
```

Press `Ctrl+C` during `analyze` to cancel cleanly.

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
