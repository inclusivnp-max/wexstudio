#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
python3 tests/release_e2e.py "${1:-tests/fixtures/compose-android}" "${2:-.verification/native-release}" ${3:+"$3"}
