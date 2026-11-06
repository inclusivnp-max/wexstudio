#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:?fresh output directory required}"; shift
for phase in initial reopen; do
    xvfb-run -a -s '-screen 0 1600x1100x24 -nolisten tcp' python3 tests/breakpoints_window_e2e.py "$out" --phase "$phase" "$@"
done
