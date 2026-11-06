#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/sdk-cache}"
cargo build --bins --features desktop --locked
python3 tests/sdk_cache_e2e.py "$OUT"
