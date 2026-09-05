# Requirements catalog — Space key dispatch

Correspondence revalidation base: `bc90f95` (plus recorded working-tree inputs).
Oracle: `crates/sakura-engine/src/space_key_dispatch_oracle.rs`

Severity: release-blocking unless marked otherwise.

The old `f26191a` revision did not identify the evaluated requirements tree.
Its verdict is preserved as historical evidence, not a current result.
`traceability.json` separates incomplete current evidence from that history.

The oracle's `AbandonContext` means dropping a context with a live reading and
reopening the connection. Its adapter uses `Dispatcher::reset`; it is not an
orderly replacement after commit/cancel. The latter loses no live reading and
arms no credit, as `replacing_a_context_after_a_commit_absorbs_no_space` checks.
`CrashRestart` is a connection reset with the shared engine fence surviving;
it does not establish whole-engine process crash/restart behavior. `Type`
represents an accepted reading event: the model may have multiple live readings,
but that does not assert that sequential raw idle-peer letters are accepted by
the product's live-peer policy. These bounds are part of the correspondence.

A live peer owns its own absorption and does not spend pending teardown credit.
A disconnected reading can still owe credit to another live idle connection of
the same host group. The oracle and test adapter now have regressions for those
distinctions. A boolean pending credit models a coalesced outstanding loss; this
campaign does not qualify an arbitrary number of distinct losses as separate
banked credits. The existing TLA SpaceKeyDispatch model still omits that credit;
its separately rerun finite searches must not be quoted as REQ-SPACE-09 proof.

| ID | Statement | Terminal owner | Persistence | Exclusions |
|---|---|---|---|---|
| REQ-SPACE-01 | Composing/converting Space converts and does not insert a document space | engine session of that connection | none | Direct mode |
| REQ-SPACE-02 | Idle Japanese Space inserts one fullwidth space when no live peer is converting and no teardown absorption is owed (REQ-SPACE-09) | host document via commit | none | Direct; half-width policy |
| REQ-SPACE-03 | One physical Space must not both insert a document space and convert | the physical key | none | none |
| REQ-SPACE-04 | Idle Space while a peer is composing/converting is absorbed | the composing connection | none | two unrelated processes |
| REQ-SPACE-05 | Commit/Cancel/abandon return that connection to Idle; orderly replacement after settlement loses no live reading | engine session | none | none |
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
