#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLS="${WEX_TOOLS_HOME:-$ROOT/.wex}"
export WEX_TOOLS_HOME="$TOOLS"
CLASSPATH="$(python3 "$ROOT/scripts/prepare-bridge.py")"
exec java -Xmx512m "-Dwex.bridge.init=$ROOT/bridge/generated.init.gradle" "-Dwex.bridge.classes=$TOOLS/bridge/classes" -cp "$CLASSPATH" dev.wex.bridge.GradleBridge "$@"
