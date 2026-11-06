#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/module-scope}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
python3 tests/module_scope_e2e.py "$OUT"
export WEX_SCOPE_PROJECT="$OUT/project" PATH="$OUT/traps:$PATH" WEX_GRADLE_BRIDGE="$OUT/traps/bridge"
WEX_K2_SYSTEM_DIR="$OUT/k2-negative" WEX_SCOPE_OUTPUT="$OUT/k2-negative.json" WEX_SCOPE_VISIBLE=0 cargo test --locked --features desktop --test module_scope_k2 -- --ignored --nocapture > "$OUT/k2-negative.log" 2>&1
python3 tests/module_scope_e2e.py "$OUT" positive
WEX_K2_SYSTEM_DIR="$OUT/k2-positive" WEX_SCOPE_OUTPUT="$OUT/k2-positive.json" WEX_SCOPE_VISIBLE=1 cargo test --locked --features desktop --test module_scope_k2 -- --ignored --nocapture > "$OUT/k2-positive.log" 2>&1
python3 tests/module_scope_e2e.py "$OUT" siblings
python3 tests/module_scope_e2e.py "$OUT" exclusion
WEX_SCOPE_PROJECT="$OUT/exclusion-project" WEX_K2_SYSTEM_DIR="$OUT/k2-exclusion-negative" WEX_SCOPE_POSITIVE=SharedApi WEX_SCOPE_FORBIDDEN=TransitiveApi WEX_SCOPE_OUTPUT="$OUT/k2-exclusion-negative.json" WEX_SCOPE_VISIBLE=0 cargo test --locked --features desktop --test module_scope_k2 -- --ignored --nocapture > "$OUT/k2-exclusion-negative.log" 2>&1
python3 tests/module_scope_e2e.py "$OUT" exclusion-positive
WEX_SCOPE_PROJECT="$OUT/exclusion-project" WEX_K2_SYSTEM_DIR="$OUT/k2-exclusion-positive" WEX_SCOPE_POSITIVE=SharedApi WEX_SCOPE_FORBIDDEN=TransitiveApi WEX_SCOPE_OUTPUT="$OUT/k2-exclusion-positive.json" WEX_SCOPE_VISIBLE=1 cargo test --locked --features desktop --test module_scope_k2 -- --ignored --nocapture > "$OUT/k2-exclusion-positive.log" 2>&1
test ! -e "$OUT/traps/FORBIDDEN_EXECUTED"
echo 'MODULE_SCOPE_E2E_OK: actual compiler/K2 reject undeclared parent dependency and accept explicit edge; transitive dependency resolves; unused AAR excluded from APK'
