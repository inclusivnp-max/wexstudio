#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
if [[ "${2:-}" == "--desktop-only" ]]; then
  python3 tests/git_flow_window_e2e.py "${1:-.verification/git-flow-live}" --desktop-only
else
  python3 tests/git_flow_window_e2e.py "${1:-.verification/git-flow-live}" --serial "${2:?explicit mtvm serial or --desktop-only required}"
fi
