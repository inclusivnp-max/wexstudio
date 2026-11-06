#!/usr/bin/env bash
# Explicit opt-in: this installs/starts only the checked-in Android fixture.
set -euo pipefail
cd "$(dirname "$0")/.."
SERIAL="${1:?Usage: scripts/check-phone.sh SERIAL}"
BIN="${WEX_BIN:-./target/debug/wex}"
IDE="${WEX_IDE:-./target/debug/wex-ide}"
OUT="${WEX_PHONE_REPORT_DIR:-.verification/stage4}"
PROJECT="${WEX_PHONE_PROJECT:-tests/fixtures/native-android}"
mkdir -p "$OUT"
"$BIN" device-info --serial "$SERIAL" > "$OUT/physical-device.json"
python3 - "$OUT/physical-device.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['emulator'] is False, 'physical phone required'
print('PHONE: %s · API %s · physical' % (x['model'],x['sdk']))
PY
"$BIN" run "$PROJECT" --serial "$SERIAL" --offline --log-seconds 5 --require-marker WEX_RUN_OK > "$OUT/phone-release.json" 2> "$OUT/phone-release-build.log"
env -u WAYLAND_DISPLAY timeout 255s "$IDE" "$PROJECT" --run-smoke "$OUT/gui-phone-release.png" --serial "$SERIAL" > "$OUT/gui-phone-release.log" 2>&1
python3 - "$SERIAL" "$OUT" <<'PY'
import json,sys
from pathlib import Path
serial,out=sys.argv[1],Path(sys.argv[2])
cli=json.loads((out/'phone-release.json').read_text())
gui=json.loads((out/'gui-phone-release.json').read_text())
assert cli['result']=='RUN_OK'
assert gui['result']=='GUI_PHONE_RUN_OK'
for r,entries in [(cli['deployment'],cli['logcat']['entries']), (gui['deployment'],gui['entries'])]:
    assert r['build_backend']=='wex-native-v1'
    assert r['native_build']['kotlin_files']>=2 and r['native_build']['java_files']>=1
    assert all(s['exit_code']==0 for s in r['native_build']['steps'])
    assert r['serial']==serial and r['device']['emulator'] is False
    assert r['apk']['application_id']=='dev.wex.android.integration'
    assert r['install']['output']['exit_code']==0 and r['install']['output']['stdout'].strip()=='Success'
    assert r['launch']['exit_code']==0 and 'Status: ok' in r['launch']['stdout']
    assert r['pid']>0 and all(e['pid']==r['pid'] for e in entries)
    for marker in ['WEX_RUN_OK', 'WEX_ACTIVITY_RESUMED', 'WEX_NATIVE_OK']:
        assert any(marker in e['message'] and r['run_id'] in e['message'] for e in entries),marker
    print('VERIFIED: build -> install Success -> launch Status: ok -> Logcat · PID %d' % r['pid'])
assert cli['deployment']['run_id'] != gui['deployment']['run_id']
assert cli['deployment']['install']['blake3'] == gui['deployment']['install']['blake3']
assert (out/'gui-phone-release.png').stat().st_size>10000
print('PHONE_E2E_OK: native Kotlin/Java/resources/DEX/sign + CLI + GPU IDE · same APK · fresh PID/runId markers')
PY
