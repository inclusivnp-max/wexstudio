#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/analysis}"
mkdir -p "$OUT"; OUT="$(cd "$OUT" && pwd)"
cargo build --bins --features desktop --locked
python3 tests/analysis_e2e.py "$OUT/native"
BIN="${WEX_BIN:-$PWD/target/debug/wex}"
for variant in debug release; do
 WEX_K2_SYSTEM_DIR="$OUT/k2-$variant" "$BIN" k2-check "$OUT/native/project" --variant "$variant" > "$OUT/k2-$variant.json" 2> "$OUT/k2-$variant.log"
done
python3 - "$OUT" <<'PY'
from pathlib import Path
from urllib.parse import urlparse,unquote
import json,sys
out=Path(sys.argv[1]);project=out/'native/project'
for variant in ['debug','release']:
 r=json.loads((out/f'k2-{variant}.json').read_text());assert r['result']=='K2_ANDROID_OK' and r['variant']==variant and r['disk_source_unchanged']
 locations=r['build_config_definition'];assert locations
 path=Path(unquote(urlparse(locations[0]['uri']).path));assert path.is_relative_to(project/'.wex/analysis'/variant)
 assert ('DEBUG = '+str(variant=='debug').lower()) in path.read_text()
 assert r['introduced_error_diagnostics']['items'] and not any(d['severity']==1 for d in r['recovered_diagnostics']['items'])
assert not (project/'.wex/native').exists()
print('ANALYSIS_K2_OK: actual K2 completion/shared-source definition/new R field/type error/recovery; generated BuildConfig true/false; debug/release before any APK')
PY
python3 - "$OUT" <<'PY'
from pathlib import Path
import shutil,sys
out=Path(sys.argv[1]);p=out/'live-project'
shutil.copytree('tests/fixtures/native-android',p,ignore=shutil.ignore_patterns('.wex'))
(p/'wex.lock.json').unlink(missing_ok=True)
PY
IDE="${WEX_IDE:-$PWD/target/debug/wex-ide}"
env -u WAYLAND_DISPLAY -u WEX_REDUCED_MOTION PATH="$OUT/native/traps:$PATH" WEX_GRADLE_BRIDGE="$OUT/native/traps/bridge" WEX_K2_SYSTEM_DIR="$OUT/k2-window" timeout 280s "$IDE" "$OUT/live-project" --analysis-smoke "$OUT/window" > "$OUT/window.log" 2>&1
python3 tests/analysis_window_test.py "$OUT"
test ! -e "$OUT/native/traps/FORBIDDEN_EXECUTED"
