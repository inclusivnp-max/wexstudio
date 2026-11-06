#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
python3 tests/terminal_window_e2e.py "${1:-.verification/terminal-live}"
