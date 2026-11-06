#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
CARGO_BUILD_JOBS=2 cargo build --bins --features desktop --locked
python3 tests/file_open_e2e.py "${1:-.verification/file-open}"
