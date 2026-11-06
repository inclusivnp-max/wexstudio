#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --features desktop --bin wex-ide --locked
PROJECT="${1:-tests/fixtures/native-android}"
OUT="${2:-.verification/editor-live}"
BIN="${WEX_EDITOR_BINARY:-./target/debug/wex-ide}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
rm -f "$OUT/editor-window.json"
env -u WAYLAND_DISPLAY -u WEX_REDUCED_MOTION WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_GRADLE_BRIDGE=/nonexistent timeout 200s "$BIN" "$PROJECT" --editor-smoke "$OUT"
python3 - "$OUT" <<'PY'
from pathlib import Path
import json,struct,sys
root=Path(sys.argv[1]);r=json.loads((root/'editor-window.json').read_text())
assert r['result']=='GUI_EDITOR_OK' and r['protected_files_unchanged']
assert not r['deployment_invoked'] and len(r['screenshots'])==10
assert r['source_lines']>10000 and r['rendered_rows']<100
assert len(r['external_process_operations'])==4
for operation in r['external_process_operations']:
    assert operation['exit']==0 and operation['output']=='EXTERNAL_WRITE_OK'
for shot in r['screenshots']:
    data=Path(shot['path']).read_bytes()
    assert data[:8]==b'\x89PNG\r\n\x1a\n'
    assert struct.unpack('>II',data[16:24])==(shot['width'],shot['height'])
for path in r['owned_scratch_files']:
    assert not Path(path).exists(), 'scratch file not cleaned: '+path
print('EDITOR_ARTIFACTS_OK: 10 native GPU screenshots; OS clipboard; IME events/candidate rect; external-process disk edits; scratch files cleaned')
PY
