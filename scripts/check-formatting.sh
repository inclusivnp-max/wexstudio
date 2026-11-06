#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:?fresh output directory required}"
python3 tests/formatting_e2e.py "$OUT"
