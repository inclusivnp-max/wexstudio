#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
BIN="${WEX_BIN:-$ROOT/target/debug/wex}"
OUT="$ROOT/.verification/stage3"
mkdir -p "$OUT"
/usr/bin/env PATH=/nonexistent "$BIN" resolve "$ROOT/tests/fixtures/android" --offline --locked > "$OUT/native-offline.json"
python3 - "$OUT/native-offline.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['schema']==1 and len(x['artifacts'])==20 and len(x['files'])==43
coordinates={':'.join(a['coordinate'][k] for k in ['group','artifact','version']) for a in x['artifacts']}
assert 'androidx.core:core:1.13.0' in coordinates
assert 'org.jetbrains.kotlin:kotlin-stdlib:2.4.0' in coordinates
print('NATIVE_RESOLVE_OK: 20 artifacts; 43 files; locked offline; no Gradle/Java/curl')
PY
