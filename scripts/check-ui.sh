#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --features desktop --bin wex-ide --locked
PROJECT="${1:-tests/fixtures/native-android}"
OUT="${2:-.verification/expressive-live}"
BIN="${WEX_UI_BINARY:-./target/debug/wex-ide}"
mkdir -p "$OUT"
rm -f "$OUT/live-window.json"
env -u WAYLAND_DISPLAY -u WEX_REDUCED_MOTION WEX_GRADLE_BRIDGE=/nonexistent timeout 100s "$BIN" "$PROJECT" --ui-smoke "$OUT"
python3 - "$OUT" <<'PY'
from pathlib import Path
import json,struct,sys
root=Path(sys.argv[1]);r=json.loads((root/'live-window.json').read_text())
assert r['result']=='GUI_EXPRESSIVE_OK' and r['disk_source_unchanged']
assert not r['deployment_invoked'] and len(r['screenshots'])==8
interrupted=dict(r['interrupted_keys'])
assert interrupted['explorer-width']>0 and interrupted['dock-tab-x']>0
for shot in r['screenshots']:
    p=Path(shot['path']);data=p.read_bytes()
    assert data[:8]==b'\x89PNG\r\n\x1a\n'
    assert struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
print('UI_ARTIFACTS_OK: 8 native GPU screenshots; interrupted explorer/dock springs; source unchanged')
PY
