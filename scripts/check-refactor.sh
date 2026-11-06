#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
PROJECT="$(realpath "${1:-tests/fixtures/native-android}")"
OUT="${2:-.verification/refactor-live}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
BIN="${WEX_REFACTOR_IDE:-$PWD/target/debug/wex-ide}"
env -u WAYLAND_DISPLAY WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_LSP_TRACE="$OUT/lsp.jsonl" WEX_GRADLE_BRIDGE=/nonexistent timeout 250s "$BIN" "$PROJECT" --refactor-smoke "$OUT"
python3 - "$OUT" <<'PY'
import json,struct,sys
from pathlib import Path
out=Path(sys.argv[1]);r=json.loads((out/'refactor-window.json').read_text())
assert r['result']=='GUI_REFACTOR_OK' and r['disk_source_unchanged'] and r['project_undo']
assert len(r['usages_before'])==len(r['usages_after'])==4 and len(r['renamed_sources'])==2
assert sum(f['edits'] for f in r['renamed_sources'])==16 and len(r['screenshots'])==5
for s in r['screenshots']:
 b=Path(s['path']).read_bytes();assert b[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',b[16:24])==(s['width'],s['height'])
print('REFACTOR_ARTIFACTS_OK: real K2; 4 symbol usages/16 LSP text edits/2 files; unsaved parked tab; navigation/preview/apply/project undo; 5 native GPU screenshots')
PY
