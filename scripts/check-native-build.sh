#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --bins --features desktop --locked
python3 tests/native_build_e2e.py
./target/debug/wex k2-check tests/fixtures/native-android > .verification/stage4/native-k2.json 2> .verification/stage4/native-k2.log
env -u WAYLAND_DISPLAY WEX_GRADLE_BRIDGE=/nonexistent timeout 150s ./target/debug/wex-ide tests/fixtures/native-android --k2-smoke .verification/stage4/native-k2-gpu.png
