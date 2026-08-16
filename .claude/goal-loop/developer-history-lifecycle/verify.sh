#!/usr/bin/env bash
# Independent rubric check for developer-history-lifecycle (C1–C8).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
RUBRIC="$ROOT/.claude/goal-loop/developer-history-lifecycle/rubric.md"
fail=0
pass() { echo "PASS $1"; }
failc() { echo "FAIL $1 — $2"; fail=1; }

[[ -f "$RUBRIC" ]] || failc C0 "rubric missing"

oracle="$ROOT/crates/sakura-engine/src/developer_history_oracle.rs"
tests="$ROOT/crates/sakura-engine/src/developer_history_oracle_tests.rs"
if [[ -f "$oracle" ]] && ! rg -q 'use crate::(dispatch|session|input_history)' "$oracle"; then
  pass C1-oracle-isolation
else
  failc C1 "oracle missing or imports production modules"
fi
if rg -q 'forbidden_stale_inactive|observed_stale_inactive' "$tests"; then
  pass C1-forbidden-example
else
  failc C1 "forbidden stale-inactive example missing"
fi

if [[ -f "$ROOT/verification/developer-history/pbt-seed.txt" \
   && -f "$ROOT/verification/developer-history/pbt-shrunk-counterexample.md" ]]; then
  pass C2-pbt-artifacts
else
  failc C2 "pbt artifacts missing"
fi

if rg -q 'input_history: Option<Arc<InputHistoryService>>' "$ROOT/crates/sakura-engine/src/server.rs" \
  && rg -q 'set_input_history\(&mut self, input_history: Option' "$ROOT/crates/sakura-engine/src/dispatch.rs" \
  && rg -q 'waiting-for-attach' "$ROOT/crates/sakura-settings/src/cli.rs" \
  && ! rg -q '=> "restart-required-to-enable"' "$ROOT/crates/sakura-settings/src/cli.rs" \
  && rg -q 'await_developer_history_terminal' "$ROOT/crates/sakura-settings/src/cli.rs"; then
  pass C3-hot-enable
else
  failc C3 "hot-enable wiring or CLI terminal incomplete"
fi

if [[ -f "$ROOT/verification/developer-history/coverage/c2-report.md" ]]; then
  pass C4-c2-report
else
  failc C4 "c2-report missing"
fi

if [[ -f "$ROOT/verification/developer-history/mutation-report.md" ]] \
  && rg -q 'Surviving mutants|Equivalent' "$ROOT/verification/developer-history/mutation-report.md"; then
  pass C5-mutation
else
  failc C5 "mutation-report incomplete"
fi

if [[ -f "$ROOT/verification/developer-history/boundary-inventory.md" ]] \
  && rg -q 'live_engine_hot_enables_developer_history' "$ROOT/crates/sakura-engine/tests/pipe_round_trip.rs" \
  && rg -q 'hot_attach_and_detach_of_developer_history' "$ROOT/crates/sakura-engine/src/dispatch.rs"; then
  pass C6-boundary
else
  failc C6 "boundary inventory or contract tests missing"
fi

if [[ -f "$ROOT/verification/tla/DeveloperHistory.tla" \
   && -f "$ROOT/verification/developer-history/tla-record.md" ]] \
  && [[ -f "$ROOT/verification/developer-history/tlc/DeveloperHistory-small/stdout.log" ]] \
  && rg -q 'No error has been found' "$ROOT/verification/developer-history/tlc/DeveloperHistory-small/stdout.log" \
  && rg -q 'No error has been found' "$ROOT/verification/developer-history/tlc/DeveloperHistory-boundary/stdout.log" \
  && rg -q 'No error has been found' "$ROOT/verification/developer-history/tlc/DeveloperHistory-concurrent/stdout.log"; then
  pass C7-tlc
else
  failc C7 "TLA/TLC record incomplete"
fi

canvas="$HOME/.cursor/projects/workspace/canvases/developer-history-verification.canvas.tsx"
if [[ -f "$ROOT/verification/developer-history/correspondence-and-audit.md" && -f "$canvas" ]] \
  && rg -q '104,163|18837|368,986' "$canvas"; then
  pass C8-audit-canvas
else
  failc C8 "correspondence audit or canvas missing real numbers"
fi

if [[ "$fail" -eq 0 ]]; then
  echo "RUBRIC PASS"
  exit 0
fi
echo "RUBRIC FAIL"
exit 1
