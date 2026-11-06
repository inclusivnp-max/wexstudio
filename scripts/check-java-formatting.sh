#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
python3 tests/java_format_e2e.py "${1:?fresh output directory required}"
