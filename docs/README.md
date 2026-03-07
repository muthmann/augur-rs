# Documentation

This repository keeps user-facing documentation separate from deeper technical notes. The same user-facing material is mirrored into `book/src/` so GitHub Pages can publish it as an mdBook.

## User Guides

- [Getting started](./getting-started.md): build the workspace, connect hardware, and run the first probe, preview, and recording flows
- [Configuration reference](./configuration.md): TOML structure, field meanings, and hardware-specific limits
- [CLI usage](./cli.md): command overview and common command sequences
- [GUI usage](./gui.md): preview workflow, runtime settings, plugin manager workflow, and ROI-grid tooling
- [Recording format](./recording.md): `.raw` output, sidecar TOML files, and runtime behavior during capture
- [Performance notes](./performance.md): what the current implementation can say about throughput and reliability, and what still needs benchmarking
- [Release notes](./releases.md): current distribution status, release artifacts, and packaging expectations

## Technical Notes

- [Feature notes](./features/README.md): capability summaries and implementation-oriented behavior notes
- [Architecture decisions](./adr/README.md): long-lived design choices and tradeoffs
