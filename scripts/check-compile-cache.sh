#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/compile-cache}"
cargo build --bins --features desktop --locked
python3 tests/compile_cache_e2e.py "$OUT"
python3 tests/compile_cache_artifacts.py "$OUT"
