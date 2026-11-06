#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
python3 tests/project_replace_ux_window_e2e.py "${1:-.verification/project-replace-ux-live}" --serial "${2:?explicit mtvm serial required}"
