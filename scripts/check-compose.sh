#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
PROJECT="$(realpath "${1:-tests/fixtures/compose-android}")"
OUT="${2:-.verification/compose}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
if [[ $# -ge 3 ]]; then [[ "$3" =~ ^emulator-[0-9]+$ ]] || { echo 'Explicit emulator serial required' >&2; exit 2; }; fi
TRAPS="$(mktemp -d)"
cleanup() {
  status=$?
  if [[ $# -ge 1 ]]; then
    python3 - "$PROJECT" "$1" <<'PYCLEAN' || status=1
import json,sys
from pathlib import Path
sys.path.insert(0,'tests');from adb_direct import Adb
package=json.loads((Path(sys.argv[1])/'wex.json').read_text())['android']['application_id']
a=Adb(sys.argv[2]);a.shell('am','force-stop','--user','current',package)
assert not a.shell('pidof',package,check=False)['stdout'].strip()
PYCLEAN
  fi
  test ! -e "$TRAPS/FORBIDDEN_EXECUTED" || status=1
  rm -rf "$TRAPS"
  exit "$status"
}
if [[ $# -ge 3 ]]; then trap 'cleanup "$SERIAL"' EXIT; SERIAL="$3"; else trap 'cleanup' EXIT; fi
for name in gradle gradlew mvn bridge; do
  printf '#!/bin/sh\nprintf forbidden > "%s/FORBIDDEN_EXECUTED"\nexit 99\n' "$TRAPS" > "$TRAPS/$name"; chmod +x "$TRAPS/$name"
done
export PATH="$TRAPS:$PATH" WEX_GRADLE_BRIDGE="$TRAPS/bridge"
cargo build --bins --features desktop --locked
python3 tests/compose_build_e2e.py "$PROJECT" "$OUT/build"
env WEX_K2_SYSTEM_DIR="$OUT/semantic-system" WEX_LSP_TRACE="$OUT/semantic-lsp.jsonl" timeout 240s "${WEX_BIN:-$PWD/target/debug/wex}" k2-check "$PROJECT" > "$OUT/semantic.json" 2> "$OUT/semantic.log"
python3 - "$OUT" <<'PYK2'
import json,sys
from pathlib import Path
r=json.loads(Path(sys.argv[1],'semantic.json').read_text());assert r['result']=='K2_COMPOSE_OK' and r['disk_source_unchanged']
assert r['composable_definition'] and r['java_definition']
assert any(d['code']=='COMPOSABLE_INVOCATION' for d in r['invalid_composable_call']['items'])
assert all(d.get('severity')!=1 for key in ['clean_main','clean_shared','recovered','final_recovered'] for d in r[key]['items'])
print('K2_COMPOSE_OK: real compiler plugin; shared composable completion/definition; Java definition; clean sources; Int/String and invalid composable-call diagnostics; recovery; disk unchanged')
PYK2

if [[ $# -ge 3 ]]; then
  SERIAL="$3"
  python3 tests/compose_device_e2e.py "$PROJECT" "$OUT/device" "$SERIAL"
  env -u WAYLAND_DISPLAY WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_LSP_TRACE="$OUT/ide-lsp.jsonl" timeout 220s "${WEX_IDE:-$PWD/target/debug/wex-ide}" "$PROJECT" --run-smoke "$OUT/compose-ide.png" --serial "$SERIAL" > "$OUT/ide.log" 2>&1
  python3 - "$OUT" "$SERIAL" <<'PY'
import json,sys,struct
from pathlib import Path
p=Path(sys.argv[1]);serial=sys.argv[2];r=json.loads((p/'compose-ide.json').read_text());d=r['deployment']
assert r['run_button_clicked'] and 'pointer' in r['input']
assert r['k2_diagnostics_observed'] and all(d.get('severity')!=1 for d in r['k2_diagnostics'])
assert r['result']=='GUI_EMULATOR_RUN_OK' and d['serial']==serial and d['device']['emulator']
assert d['build_backend']=='wex-native-v1' and d['native_build']['compose']['modules']==['shared','app']
assert any(e['pid']==d['pid'] and 'WEX_COMPOSE_FRAME count=0' in e['message'] and d['run_id'] in e['message'] for e in r['entries'])
b=(p/'compose-ide.png').read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n';assert min(struct.unpack('>II',b[16:24]))>=480
sys.path.insert(0,'tests');from adb_direct import Adb
a=Adb(serial);a.shell('am','force-stop','--user','current',d['apk']['application_id'])
assert not a.shell('pidof',d['apk']['application_id'],check=False)['stdout'].strip()
print('COMPOSE_IDE_OK: native Run button click -> real Compose first frame -> PID/runId Logcat; native GPU screenshot; explicit mtvm serial; Stop PID gone')
PY
fi
