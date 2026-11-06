#!/usr/bin/env bash
# Native Android checks only. Does not invoke Gradle or provision distributions.
set -euo pipefail
cd "$(dirname "$0")/.."
exec ./scripts/check-native-build.sh
