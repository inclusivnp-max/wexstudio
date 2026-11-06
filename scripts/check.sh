#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo test --all-targets --features desktop --locked
cargo clippy --all-targets --features desktop --locked -- -D warnings
cargo build --bins --features desktop --locked
python3 tests/cli_smoke.py
python3 tests/native_window_process.py
