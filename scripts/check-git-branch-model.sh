#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
if [[ "${2:?explicit --desktop-only or emulator-5580 required}" == --desktop-only ]]; then
  python3 tests/git_branch_model_e2e.py "${1:?output required}" --desktop-only
else
  python3 tests/git_branch_model_e2e.py "${1:?output required}" --serial "$2"
fi
