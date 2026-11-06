#!/usr/bin/env bash
# Host regression is separate evidence from check-debug.sh EXPLICIT_SERIAL.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/debug-host}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
TRAPS="$(mktemp -d)"; trap 'rm -rf "$TRAPS"' EXIT
for name in gradle gradlew mvn bridge adb emulator; do
 printf '#!/bin/sh\nprintf forbidden > "%s/FORBIDDEN_EXECUTED"\nexit 99\n' "$TRAPS" > "$TRAPS/$name"
 chmod +x "$TRAPS/$name"
done
export PATH="$TRAPS:$PATH" WEX_GRADLE_BRIDGE="$TRAPS/bridge"
python3 scripts/prepare-debugger.py --offline
python3 tests/debugger_tools_test.py
python3 tests/debugger_jvm_e2e.py "$OUT/jvm-debug.json"
WEX_K2_SYSTEM_DIR="$OUT/k2-system" bash scripts/check-ui.sh tests/fixtures/native-android "$OUT/expressive-ui"
test ! -e "$TRAPS/FORBIDDEN_EXECUTED"
python3 - "$OUT" <<'PY'
from pathlib import Path
import json,sys
root=Path(sys.argv[1]);j=json.loads((root/'jvm-debug.json').read_text());ui=json.loads((root/'expressive-ui/live-window.json').read_text())
assert j['result']=='JVM_DEBUG_OK' and j['source_unchanged']
assert len(j['snapshots']['breakpoint']['frames'])==73
assert any(v['name']=='depth' and v['value']=='60' for v in j['snapshots']['deepCaller']['variables'])
assert ui['result']=='GUI_EXPRESSIVE_OK' and not ui['deployment_invoked']
(root/'host-debug.json').write_text(json.dumps({'result':'DEBUG_HOST_OK','jvm':str(root/'jvm-debug.json'),'ui':str(root/'expressive-ui/live-window.json'),'android_phone_tested':False,'deployment_invoked':False,'forbidden_executables_invoked':False},indent=2)+'\n')
print('DEBUG_HOST_OK: real JDK21 JDWP/DAP, 73 frames/deep caller/steps/locals; native Material 3 Expressive window; no Gradle/Maven/ADB executable; no phone deployment')
PY
