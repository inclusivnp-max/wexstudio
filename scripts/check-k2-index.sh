#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
cargo build --bins --features desktop --locked
python3 tests/k2_index_e2e.py "${1:?fresh output directory required}"
