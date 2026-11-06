#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-.verification/history-bench}"
mkdir -p "$OUT"; OUT="$(realpath "$OUT")"
cargo build --release --lib --features desktop --locked --message-format=json > "$OUT/build.jsonl"
LIB="$(python3 - "$OUT/build.jsonl" <<'PY'
import json,sys
paths=[]
for line in open(sys.argv[1]):
 x=json.loads(line)
 if x.get('reason')=='compiler-artifact'and x['target']['name']=='wexandroidstudio':
  paths += [p for p in x['filenames']if p.endswith('.rlib')]
assert len(paths)==1,paths
print(paths[0])
PY
)"
sha256sum "$LIB" > "$OUT/library.sha256"
DEP="$(dirname "$LIB")"
if [[ "$(basename "$DEP")" != deps ]]; then DEP="$DEP/deps"; fi
rustc --edition=2024 -O -C panic=abort tests/support/history_benchmark.rs --extern "wexandroidstudio=$LIB" -L "dependency=$DEP" -o "$OUT/benchmark"
"$OUT/benchmark" | tee "$OUT/benchmark.json"
