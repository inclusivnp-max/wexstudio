#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/preview-discovery}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
TRAPS="$(mktemp -d)"; trap 'rm -rf "$TRAPS"' EXIT
for name in gradle gradlew mvn bridge adb emulator; do
 printf '#!/bin/sh\nprintf forbidden > "%s/FORBIDDEN_EXECUTED"\nexit 99\n' "$TRAPS" > "$TRAPS/$name";chmod +x "$TRAPS/$name"
done
export PATH="$TRAPS:$PATH" WEX_GRADLE_BRIDGE="$TRAPS/bridge"
BIN="${WEX_BIN:-$PWD/target/debug/wex}";IDE="${WEX_IDE:-$PWD/target/debug/wex-ide}"
"$BIN" prepare-preview tests/fixtures/compose-android --offline > "$OUT/prepare.log" 2>&1
python3 tests/preview_bytecode_test.py > "$OUT/bytecode.log" 2>&1
python3 tests/preview_discovery_e2e.py "$OUT/cli"
env -u WAYLAND_DISPLAY WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_LSP_TRACE="$OUT/lsp.jsonl" timeout 700s "$IDE" "$OUT/cli/project" --preview-catalog-smoke "$OUT/ide" > "$OUT/ide.log" 2>&1
python3 tests/preview_discovery_window_test.py "$OUT"
test ! -e "$TRAPS/FORBIDDEN_EXECUTED"
