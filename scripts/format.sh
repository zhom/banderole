#!/usr/bin/env bash
set -euo pipefail

# Run rustfmt first for quick formatting
cargo fmt --all

# Run clippy across all targets and features; fail on any warning
cargo clippy --fix --allow-dirty --all-targets --all-features -- -D warnings -D clippy::all

# Additionally, run clippy for tests explicitly to catch test-only lints
cargo clippy --fix --allow-dirty --tests -- -D warnings -D clippy::all