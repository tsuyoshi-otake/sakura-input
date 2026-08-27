# Requirements catalog — Space key dispatch

Revision: `f26191aa16a6b3569cdf004e4852650f7de1a17f`
Oracle: `crates/sakura-engine/src/space_key_dispatch_oracle.rs`

Severity: release-blocking unless marked otherwise.

REQ-SPACE-09 was added after the revision recorded above, and it inverted what
the REQ-SPACE-06 tests asserted about the Space following a crash/restart. The
oracle, its C2 atoms (now 11) and the Rust tests were changed together and all
pass. The TLA+ spec, the TLC configurations, the cargo-mutants run and the
hashes in `traceability.json` still describe the pre-#102 oracle, and need a
separate re-run before that record can be quoted again.

| ID | Statement | Terminal owner | Persistence | Exclusions |
|---|---|---|---|---|
| REQ-SPACE-01 | Composing/converting Space converts and does not insert a document space | engine session of that connection | none | Direct mode |
| REQ-SPACE-02 | Idle Japanese Space inserts one fullwidth space when no live peer is converting and no teardown absorption is owed (REQ-SPACE-09) | host document via commit | none | Direct; half-width policy |
| REQ-SPACE-03 | One physical Space must not both insert a document space and convert | the physical key | none | none |
| REQ-SPACE-04 | Idle Space while a peer is composing/converting is absorbed | the composing connection | none | two unrelated processes |
| REQ-SPACE-05 | Commit/Cancel/Replace return that connection to Idle | engine session | none | none |
| REQ-SPACE-06 | Crash/restart of a connection drops its composition | new session after reconnect | none | other live connections |
| REQ-SPACE-07 | Timeout Space is absorbed without a document write | recovery fence | none | successful SendKey |
| REQ-SPACE-08 | Dropped Space leaves composition unchanged | host | none | none |
| REQ-SPACE-09 | A teardown that dropped a *live* reading absorbs exactly one following idle Space, and nothing else. Added for #102: the DLL drops the pipe when a key exceeds its 50 ms budget, the composing session is torn down with its reading still live, and the replacement session committed the user's next Space as U+3000. Bounds: one Space per teardown, never a letter, disarmed by composing again. | replacement session | none | orderly commit/cancel/replace; Henkan; Space bound in Idle by a custom keymap |
| BND-SPACE-01 | Oracle/TLA exploration bound: at most `MAX_DOCUMENT_SPACES` (4) idle insertions. Product IME has no quota. | model document | none | product idle Space |
| BND-SPACE-02 | At most 3 modeled connections | engine accept pool | none | >3 TSF threads |
| BND-SPACE-03 | Logical time is one tick per event | oracle/TLA eventCount | none | wall clock |
| FAIL-SPACE-DUAL | Dual TSF/pipe delivery of one Space | share-nothing workers | none | single-focus hosts |
| FAIL-SPACE-DUP | Duplicate Space on idle accumulates spaces; on converting stays converting | session | none | none |
| FAIL-SPACE-OMIT | Omitted Space does not insert | session | none | none |
| FAIL-SPACE-REORDER | Space before Type inserts, then composition starts | session | none | none |
| FAIL-SPACE-CANCEL | Escape then Space inserts | session | none | none |
| FAIL-SPACE-TIMEOUT | Short budget / Busy leaves composition, no space | recovery | none | none |
| FAIL-SPACE-CRASH | Kill/restart reopens Idle | new process | none | in-flight host buffer |
| FAIL-SPACE-EXHAUST | Further idle spaces absorb after the bound | document | none | none |
| FAIL-SPACE-DEAD | Disconnected connection ignores Space | connection | none | none |
