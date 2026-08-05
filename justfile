# Task runner mirroring .github/workflows/ci.yml
# Install with: cargo install just

set windows-shell := ["powershell.exe", "-NoProfile", "-Command"]

# Run the full CI pipeline in the same order as GitHub Actions
ci: fmt-check clippy test build

# Check formatting (CI gate)
fmt-check:
    cargo fmt --all -- --check

# Apply formatting
fmt:
    cargo fmt --all

# Lint with warnings denied, including tests and benches
clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run the workspace test suite
test:
    cargo test --workspace --locked

# Build every workspace member
build:
    cargo build --workspace --locked

# Exercise the optional hdf5 feature (needs libhdf5 installed)
test-hdf5:
    cargo test --locked -p augur-core --features hdf5

# Release binaries as produced by .github/workflows/release.yml
release:
    cargo build --release --locked --bin augur --bin AugurRS
