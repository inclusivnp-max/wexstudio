#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/resource-cache}"
cargo build --bins --features desktop --locked
python3 tests/resource_cache_e2e.py "$OUT"
