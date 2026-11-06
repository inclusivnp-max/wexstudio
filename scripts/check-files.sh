#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Emulator must already be running; this suite never substitutes another transport.
[[ "${2:-}" == --serial && "${3:-}" == emulator-5580 ]] || { echo 'Usage: check-files.sh OUTPUT --serial emulator-5580'; exit 2; }
CARGO_BUILD_JOBS=2 cargo build --bins --features desktop --locked
python3 tests/file_ops_e2e.py "$1" --serial "$3"
