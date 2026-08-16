#!/usr/bin/env bash
set -euo pipefail

JAR_PATH="${1:-/tmp/sakura-tla/tla2tools.jar}"
TIMEOUT_SECONDS="${TIMEOUT_SECONDS:-180}"
WORKERS="${WORKERS:-2}"
SEED="${SEED:-20260816}"
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MODEL_DIR="$REPO_ROOT/verification/tla"
OUTPUT_ROOT="$REPO_ROOT/verification/developer-history/tlc"

if [[ ! -f "$JAR_PATH" ]]; then
  echo "TLA+ tools jar not found: $JAR_PATH" >&2
  exit 1
fi

mkdir -p "$OUTPUT_ROOT"
CONFIGS=(
  DeveloperHistory-small.cfg
  DeveloperHistory-boundary.cfg
  DeveloperHistory-concurrent.cfg
  DeveloperHistory-crash.cfg
)

for configName in "${CONFIGS[@]}"; do
  slug="${configName%.cfg}"
  runDir="$OUTPUT_ROOT/$slug"
  rm -rf "$runDir"
  mkdir -p "$runDir/states"
  stdout="$runDir/stdout.log"
  stderr="$runDir/stderr.log"
  set +e
  timeout "$TIMEOUT_SECONDS" java -cp "$JAR_PATH" tlc2.TLC \
    -config "$MODEL_DIR/$configName" \
    -workers "$WORKERS" \
    -coverage 1 \
    -fp 0 \
    -seed "$SEED" \
    -metadir "$runDir/states" \
    "$MODEL_DIR/DeveloperHistory.tla" \
    >"$stdout" 2>"$stderr"
  code=$?
  set -e
  if [[ $code -eq 124 ]]; then
    echo "timed out after ${TIMEOUT_SECONDS}s with ${WORKERS} workers" >"$runDir/timeout.txt"
    echo "TLC $configName TIMED OUT after ${TIMEOUT_SECONDS} seconds"
  else
    echo "TLC $configName exit $code"
  fi
done

echo "TLC logs written under $OUTPUT_ROOT"
