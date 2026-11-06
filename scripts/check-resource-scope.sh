#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/resource-scope}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
python3 tests/resource_scope_e2e.py "$OUT"
export WEX_RESOURCE_PROJECT="$OUT/project" PATH="$OUT/traps:$PATH" WEX_GRADLE_BRIDGE="$OUT/traps/bridge"
for module in shared app; do
 WEX_K2_SYSTEM_DIR="$OUT/k2-$module-negative" WEX_RESOURCE_MODULE="$module" WEX_RESOURCE_OUTPUT="$OUT/k2-$module-negative.json" WEX_RESOURCE_VISIBLE=0 cargo test --locked --features desktop --test resource_scope_k2 -- --ignored --nocapture > "$OUT/k2-$module-negative.log" 2>&1
done
python3 tests/resource_scope_e2e.py "$OUT" positive
for module in shared app; do
 WEX_K2_SYSTEM_DIR="$OUT/k2-$module-positive" WEX_RESOURCE_MODULE="$module" WEX_RESOURCE_OUTPUT="$OUT/k2-$module-positive.json" WEX_RESOURCE_VISIBLE=1 cargo test --locked --features desktop --test resource_scope_k2 -- --ignored --nocapture > "$OUT/k2-$module-positive.log" 2>&1
done
python3 tests/resource_scope_e2e.py "$OUT" failures
test ! -e "$OUT/traps/FORBIDDEN_EXECUTED"
echo 'RESOURCE_SCOPE_K2_OK: actual shared/app K2 completion + negative diagnostics + qualified recovery; no Gradle/network/ADB executable'
