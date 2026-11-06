#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/workspace-analysis}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
python3 tests/workspace_analysis_e2e.py "$OUT"
export WEX_WORKSPACE_PROJECT="$OUT/project" ANDROID_HOME="$OUT/sdk" JAVA_HOME="$OUT/jdk" PATH="$OUT/traps:$PATH" WEX_GRADLE_BRIDGE="$OUT/traps/bridge"
WEX_K2_SYSTEM_DIR="$OUT/k2-negative" WEX_WORKSPACE_OUTPUT="$OUT/k2-negative.json" WEX_WORKSPACE_VISIBLE=0 cargo test --locked --features desktop --test workspace_analysis_k2 genuine_k2_workspace_visibility -- --ignored --nocapture > "$OUT/k2-negative.log" 2>&1
# APK verification uses the real SDK tool inventory, not the private analysis-only copy.
env -u ANDROID_HOME -u ANDROID_SDK_ROOT -u JAVA_HOME python3 tests/workspace_analysis_e2e.py "$OUT" positive
WEX_K2_SYSTEM_DIR="$OUT/k2-positive" WEX_WORKSPACE_OUTPUT="$OUT/k2-positive.json" WEX_WORKSPACE_VISIBLE=1 cargo test --locked --features desktop --test workspace_analysis_k2 genuine_k2_workspace_visibility -- --ignored --nocapture > "$OUT/k2-positive.log" 2>&1
python3 tests/workspace_analysis_e2e.py "$OUT" compose
WEX_K2_SYSTEM_DIR="$OUT/k2-detached-compose" WEX_WORKSPACE_COMPOSE_PROJECT="$OUT/compose-project" WEX_WORKSPACE_COMPOSE_OUTPUT="$OUT/k2-detached-compose.json" cargo test --locked --features desktop --test workspace_analysis_k2 genuine_detached_compose_metadata -- --ignored --nocapture > "$OUT/k2-detached-compose.log" 2>&1
test ! -e "$OUT/traps/FORBIDDEN_EXECUTED"
echo 'WORKSPACE_K2_OK: real detached module completion/Java source definition/leaf R; parent R error and recovery; pure module diagnostics; app APK remains isolated'
