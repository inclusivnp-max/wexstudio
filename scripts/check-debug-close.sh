#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
serial="${1:?explicit ADB transport required}"
project="${2:?native Debug fixture required}"
out="${3:?fresh output directory required}"
mode="${4:-physical}"
shift 3
if [ "$#" -gt 0 ]; then shift; fi
case "$mode" in physical|virtual);; *) echo 'Expected physical or virtual' >&2; exit 2;; esac
python3 tests/debug_close_window_e2e.py "$serial" "$project" "$out" --mode "$mode" "$@"
