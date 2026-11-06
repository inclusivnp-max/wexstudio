#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
SERIAL="${1:?Usage: check-debug.sh EXPLICIT_ADB_TRANSPORT [PROJECT] [OUTPUT] [physical|virtual] [EXPECTED_HARDWARE_SERIAL]}"
PROJECT="$(realpath "${2:-tests/fixtures/native-android}")"
OUT="${3:-.verification/debug-phone}"
MODE="${4:-physical}"
case "$MODE" in physical) export WEX_DEBUG_EXPECT_EMULATOR=0;; virtual) export WEX_DEBUG_EXPECT_EMULATOR=1;; *) echo 'Expected physical or virtual device mode' >&2; exit 2;; esac
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
BIN="${WEX_BIN:-$PWD/target/debug/wex}"
IDE="${WEX_IDE:-$PWD/target/debug/wex-ide}"
# Check exact transport and hardware identity before preparing tools or changing the app.
python3 - "$SERIAL" "$MODE" "${5:-$SERIAL}" "$OUT" "$BIN" <<'PYIDENTITY'
import sys,json,subprocess
from pathlib import Path
sys.path.insert(0,'tests');from adb_direct import Adb
serial,mode,expected,out,binary=sys.argv[1:]
devices=json.loads(subprocess.check_output([binary,'devices']))
assert any(d['serial']==serial and d['state']=='device' for d in devices), 'Exact ADB transport absent: '+serial
adb=Adb(serial)
props={key:adb.shell('getprop',key)['stdout'].strip() for key in ['ro.serialno','ro.boot.serialno','ro.product.model','ro.kernel.qemu']}
assert (props['ro.kernel.qemu']=='1')==(mode=='virtual'),props
if mode=='physical':assert props['ro.serialno']==expected and props['ro.boot.serialno']==expected,props
Path(out,'device-identity.json').write_text(json.dumps({'adb_transport':serial,'expected_hardware_serial':expected,'mode':mode,'properties':props,'devices':devices},indent=2)+'\n')
print('DEBUG_DEVICE_IDENTITY_OK: hardware='+expected+'; explicit ADB transport='+serial)
PYIDENTITY
python3 scripts/prepare-debugger.py --offline
python3 tests/debugger_tools_test.py
python3 tests/debugger_jvm_e2e.py "$OUT/jvm-debug.json"
cargo build --bins --features desktop --locked
TRAPS="$(mktemp -d)"; trap 'rm -rf "$TRAPS"' EXIT
for name in gradle gradlew mvn bridge; do
 printf '#!/bin/sh\nprintf forbidden > "%s/GRADLE_OR_MAVEN_EXECUTED"\nexit 99\n' "$TRAPS" > "$TRAPS/$name"
 chmod +x "$TRAPS/$name"
done
export PATH="$TRAPS:$PATH" WEX_GRADLE_BRIDGE="$TRAPS/bridge"
python3 - "$PROJECT" "$OUT" <<'PY'
from pathlib import Path
import sys,json,hashlib
p=Path(sys.argv[1]);o=Path(sys.argv[2]);b=p/'.wex/breakpoints.json'
assert not list(p.rglob('gradlew')) and not list(p.rglob('*.gradle*'))
files={str(f):hashlib.sha256(f.read_bytes()).hexdigest() for f in p.rglob('*') if f.suffix in ('.kt','.java') and '.wex' not in f.parts}
(o/'before.json').write_text(json.dumps({'sources':files,'prefs':hashlib.sha256(b.read_bytes()).hexdigest() if b.exists() else None}))
PY
rm -f "$OUT/cli-dap.jsonl" "$OUT/gui-dap.jsonl" "$OUT/cli-debug.json" "$OUT/debug-window.json"
WEX_DAP_TRACE="$OUT/cli-dap.jsonl" "$BIN" debug-check "$PROJECT" --serial "$SERIAL" --output "$OUT/cli-debug.json" > "$OUT/cli.stdout" 2> "$OUT/cli.log"
env -u WAYLAND_DISPLAY -u WEX_REDUCED_MOTION WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_DAP_TRACE="$OUT/gui-dap.jsonl" timeout 260s "$IDE" "$PROJECT" --debug-smoke "$OUT" --serial "$SERIAL" > "$OUT/gui.log" 2>&1
python3 - "$PROJECT" "$OUT" "$SERIAL" <<'PY'
from pathlib import Path
import json,sys,hashlib,struct,os
p=Path(sys.argv[1]);o=Path(sys.argv[2]);serial=sys.argv[3]
a=json.loads((o/'cli-debug.json').read_text());b=json.loads((o/'debug-window.json').read_text());before=json.loads((o/'before.json').read_text())
assert a['result']=='ANDROID_DEBUG_OK' and b['result']=='GUI_ANDROID_DEBUG_OK'
for r in [a,b]:
 assert r['run']['serial']==serial and r['run']['device']['emulator']==(os.environ['WEX_DEBUG_EXPECT_EMULATOR']=='1')
 transport=r['run']['debug_transport']
 assert transport['kind']=='direct-adb-preclaimed-jdwp' and transport['pid']==r['run']['pid']
 assert transport['claim_source'] in ('system-server-fork','exact-package-pidof') and 1<=transport['launch_attempt']<=3
 assert r['run']['build_backend']=='wex-native-v1' and r['run']['native_build']['kotlin_files']>=2
 assert r['run']['install']['output']['exit_code']==0 and r['disk_source_unchanged']
 assert r['stop']['shell']['exit_code']==0 and r['stop']['pid_gone']
assert a['logcat']['marker_found']
assert any('WEX_DEBUG_OK result=17' in x['message'] and b['run']['run_id'] in x['message'] for x in b['logcat'])
assert len(b['screenshots'])==13
assert [len(p['variables']) for p in a['array_pages']]==[100,100,50]
assert a['array_pages'][2]['variables'][-1]['value']=='747'
for key,count in [('array100',100),('array200',200),('array250',250)]:
 assert any(len(v)==count and v[-1]['value']==str((count-1)*3) for v in b['snapshots'][key]['variables'].values())
for s in b['screenshots']:
 data=Path(s['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n';assert struct.unpack('>II',data[16:24])==(s['width'],s['height'])
assert all(not s['stack_loading'] and s['stack_error'] is None and not s['scopes_loading'] and s['scopes_error'] is None for s in b['snapshots'].values())
assert all(s['variables_loading']==[] and s['variable_errors']=={} for s in b['snapshots'].values())
assert all(all(scope['variablesReference']==0 or str(scope['variablesReference']) in s['variables'] for scope in s['scopes']) for s in b['snapshots'].values())
all_messages=[json.loads(line.split(' ',1)[1]) for line in (o/'gui-dap.jsonl').read_text().splitlines() if line.startswith(('send ','recv '))]
stack_requests=[m for m in all_messages if m.get('type')=='request' and m.get('command')=='stackTrace']
assert stack_requests and all('levels' not in m['arguments'] and 'startFrame' not in m['arguments'] for m in stack_requests)
stack_replies=[m['body'] for m in all_messages if m.get('type')=='response' and m.get('command')=='stackTrace' and m.get('success')]
assert stack_replies and all(len(s['stackFrames'])==s['totalFrames'] for s in stack_replies)
array_ids=[int(k) for k,v in b['snapshots']['array250']['variables'].items() if len(v)==250]
assert len(array_ids)==1
trace=[json.loads(line.split(' ',1)[1]) for line in (o/'gui-dap.jsonl').read_text().splitlines() if line.startswith('send ')]
pages=[m['arguments'] for m in trace if m.get('command')=='variables' and m['arguments'].get('variablesReference')==array_ids[0]]
assert [(m['start'],m['count']) for m in pages]==[(0,100),(100,100),(200,50)],pages
for path,h in before['sources'].items():assert hashlib.sha256(Path(path).read_bytes()).hexdigest()==h,path
bp=p/'.wex/breakpoints.json';assert (hashlib.sha256(bp.read_bytes()).hexdigest() if bp.exists() else None)==before['prefs']
print('ANDROID_DEBUG_OK: '+serial+('; mtvm/ART virtual' if os.environ['WEX_DEBUG_EXPECT_EMULATOR']=='1' else '; Pixel/ART physical')+'; breakpoint seed=7 doubled=14; stepIn/next adjusted=17; stepOut return=17; complete call stack/caller/object variables; array pages=100/100/50 last=747; pause/continue/Stop; PID gone')
print('DEBUG_ARTIFACTS_OK: 13 native GPU screenshots; real Java Debug Server DAP/JDWP; fresh PID-bound Logcat; source/preferences unchanged; no Gradle/Maven')
PY
test ! -e "$TRAPS/GRADLE_OR_MAVEN_EXECUTED"
