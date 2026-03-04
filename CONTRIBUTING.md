# Contributing

Contributions are welcome, but changes should stay tight, testable, and documented.

## Before You Start

- Open an issue or start a discussion before large refactors or major feature additions
- Keep hardware scope explicit when a change is specific to EVK4 or IMX636 behavior
- Prefer small pull requests over broad mixed-purpose changes

## Development Checklist

Run the relevant checks before opening a pull request:

```bash
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

If your change touches hardware-facing behavior, include the result of any manual EVK4 validation you performed.

## Documentation Expectations

Update documentation when behavior changes:

- `README.md` for front-door usage changes
- files in `docs/` for setup, configuration, CLI, GUI, or recording changes
- `docs/features/` for deeper technical behavior notes
- `docs/adr/` for long-lived architectural decisions

## Pull Request Notes

Include:

- what changed
- how you verified it
- any hardware, platform, or sensor assumptions
