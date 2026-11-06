#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/model-reload}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
python3 tests/model_reload_e2e.py "$OUT"
ANDROID_HOME="$OUT/sdk" JAVA_HOME="$OUT/jdk" PATH="$OUT/traps:$PATH" WEX_GRADLE_BRIDGE="$OUT/traps/bridge" WEX_K2_SYSTEM_DIR="$OUT/k2-system" MODEL_RELOAD_PROJECT="$OUT/project" MODEL_RELOAD_OUTPUT="$OUT/reload-k2.json" cargo test --locked --features desktop --test model_reload_k2 -- --ignored --nocapture > "$OUT/reload-k2.log" 2>&1
test ! -e "$OUT/traps/FORBIDDEN_EXECUTED"
echo 'MODEL_RELOAD_E2E_OK: actual native watcher + SDK + K2 session replacement; automatic resource/config/module/variant refresh; last-good and dirty buffers retained; no Gradle/APK'
