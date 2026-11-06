#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
PROJECT="$(realpath "${1:-tests/fixtures/compose-android}")"
OUT="${2:-.verification/headless-preview}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
if [[ -z "${WEX_BIN:-}" || -z "${WEX_IDE:-}" ]]; then
 cargo build --bins --features desktop --locked
fi
TRAPS="$(mktemp -d)"; trap 'rm -rf "$TRAPS"' EXIT
for name in gradle gradlew mvn bridge adb emulator; do
 printf '#!/bin/sh\nprintf forbidden > "%s/FORBIDDEN_EXECUTED"\nexit 99\n' "$TRAPS" > "$TRAPS/$name"; chmod +x "$TRAPS/$name"
done
export PATH="$TRAPS:$PATH" WEX_GRADLE_BRIDGE="$TRAPS/bridge"
"${WEX_BIN:-$PWD/target/debug/wex}" resolve "$PROJECT" --offline > "$OUT/resolve.log" 2>&1
"${WEX_BIN:-$PWD/target/debug/wex}" prepare-preview "$PROJECT" --offline > "$OUT/prepare.log" 2>&1
python3 tests/preview_e2e.py "$PROJECT" "$OUT/cli"
env -u WAYLAND_DISPLAY WEX_K2_SYSTEM_DIR="$OUT/k2-system" WEX_LSP_TRACE="$OUT/ide-lsp.jsonl" timeout 450s "${WEX_IDE:-$PWD/target/debug/wex-ide}" "$PROJECT" --preview-smoke "$OUT/ide" > "$OUT/ide.log" 2>&1
python3 - "$OUT" <<'PY'
from pathlib import Path
import json,sys,struct
p=Path(sys.argv[1]);r=json.loads((p/'ide/preview-window.json').read_text())
assert r['result']=='GUI_HEADLESS_PREVIEW_OK' and r['disk_source_unchanged'] and r['snapshot_jobs_cleaned']
assert not r['deployment_invoked'] and not r['emulator_used'] and len(r['screenshots'])==6
for s in r['screenshots']:
 data=Path(s['path']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n' and struct.unpack('>II',data[16:24])==(s['width'],s['height'])
a=r['snapshots'];assert a['02-unsaved']['report']['buffers'] and a['02-unsaved']['dirty']
assert a['01-original']['report']['png_blake3']!=a['02-unsaved']['report']['png_blake3']
assert a['error']['retained_png']==a['02-unsaved']['report']['png_blake3'] and a['error']['stale']
assert a['cancel']['retained_png']==a['02-unsaved']['report']['png_blake3']
assert a['05-dark']['report']['entry']['dark'] and not a['05-dark']['dirty']
for key in ['01-original','02-unsaved','05-dark']:
 data=Path(a[key]['copied_image']).read_bytes();assert data[:8]==b'\x89PNG\r\n\x1a\n'
print('GUI_HEADLESS_PREVIEW_OK: 6 GPU screenshots; actual Compose + live unsaved Ctrl+A/Paste; compiler failure/Cancel retain last good image; dark/reopen; no emulator/deploy/disk edit')
PY
test ! -e "$TRAPS/FORBIDDEN_EXECUTED"
