# Issue #141: TDD verification

## First correction: unissued callback expiry (#102, #134)

Baseline: `ef2f021` / v1.0.36. Work is isolated from the older dirty main checkout.

### Counterexample

A synchronized TSF Engine link can decline an operation because its callback
deadline expired before sending any bytes. The baseline maps both `link()`'s
early expiry and transport `Fault::DeadlineExpired` to `Answer::Unavailable`.
The TextService caller treats that answer as recovery: it cancels preceding
writes, disconnects, and schedules rescue finalization. Keeping the link inside
Engine alone therefore does not preserve the caller's composition.

Before changing shipping code, the callback-deadline regression tests failed
with `Unavailable` instead of the expected `Rejected`: 2 passed, 2 failed,
exit 101. Tests completed and no owned runner survived.

### Change and acceptance

An expired **unissued** operation on a synchronized link now returns `Rejected`.
The existing TextService rejection path cancels only its own empty reservation.
Missing links, uncertain earlier mutations, and actual sent-request timeouts
remain recovery outcomes. No deadline is extended and no retry is added.

The bounded private-pipe regression checks public key/probe/commit/reconversion/
candidate-commit/AI-apply calls and expiry at the lower request boundary. No
rejected request or recovery command reaches the peer, request identity remains
unchanged, and the next key uses the same session. Missing and desynchronized
links are negative controls. The fixture uses synthetic input only.

### Verification commands

- `cargo test --locked --offline -p sakura-tsf --lib callback_deadline -- --test-threads=1`: RED 2 failures, then GREEN 4 passed.
- `cargo test --locked --offline -p sakura-tsf -p sakura-ipc`: passed.
- `cargo test --locked --offline --workspace`: final patch including both corrections, 1823 passed, 84 ignored.
- `cargo clippy --locked --offline --workspace --all-targets -- -D warnings`: passed after removing a large Result error variant and panic-prone test indexing.
- `cargo fmt --all -- --check`, `git diff --check`: passed for the final patch.

Every cargo invocation has an outer timeout and exact launched-process cleanup.
The runner lists remaining executables under the owned target directory after
completion. Local RED/GREEN/full-run evidence is under
`.codex/goal-loop/issue-141-tdd/` (ignored, not uploaded).

### Limits and remaining cases

This is a reproduced code-contract failure, not proof that it caused either of
the two production recovery records without a key-timeout record. Key histories
do not carry the needed request/host correlation. Case A–D recovery after an
actual sent timeout, late old-session work, case E placement timeouts, and case F
candidate quality remain separate work. No installed binary, production profile,
TSF registration, or real host was modified. The 84 ignored tests are not passes.
Neither #102 nor parent #141 is complete on the basis of this patch.

## Second correction: disconnected output publication (#102, case D boundary)

The private-pipe test pauses a real dispatched half/full-width mode-toggle
request at the Output boundary. The client times out and closes; a newer
Katakana UI revision is then published before the abandoned output resumes.
Before the correction, the old output overwrote that revision (expected 2,
observed 3). The first ordinary-character fixture did not change the mode and
therefore passed; only the mode-toggle counterexample established RED.

`serve` now encodes and successfully writes an Output response before publishing
its mode/candidate snapshot. Failed encoding or writing terminates the connection
without that publication. The regression is GREEN. The full workspace suite and
Clippy passed after this change, with zero surviving owned test processes.

This covers the failed-write Output path. A successful pipe write is not proof
that the client consumed or applied the document edit. This change does not
cancel dispatch computation, release the composition fence early, reverse
learning/history side effects, or order results across still-connected clients.
The test proves mode publication, not an observed production candidate loss.
Other response variants and placement timeouts remain separate investigation.

## Placement duplicates (#142)

The private-pipe fixture issued 104 placement requests for repeated unchanged
geometry plus a key and visibility transitions before correction (RED). The
client now remembers one acknowledged visible placement until any intervening
wire request. The same fixture sends 4 requests (GREEN), retaining repeated idle
geometry refresh because another host can replace the shared idle mode anchor.
Storage is O(1), no background queue or retry loop is introduced, and every
changed geometry or intervening request invalidates the acknowledgement.

A second bounded fixture delays an acknowledgement past the 10 ms budget. The
failed geometry is not cached; the next externally requested update is sent,
the old reply is drained by request identity, and the newest geometry can then
be deduplicated. No text-session desynchronization is introduced.

This proves redundant synthetic work, not the cause of the production
simultaneous multi-PID timeouts. Real host/DPI visual acceptance remains open.

## Short-reading repair history (#108, case F)

The production 1.0.36 code admitted an Advanced `う -> い` repair as a
CommitHistory hint merely after an unrelated `い` commit. A synthetic in-memory
learning test reproduced that failure before the fix. The conservative Rule-only
admission already present in the owner's separate dirty checkout was ported here
without modifying that checkout. Four short-reading pairs now pass, while the
existing named-rule typo-repair test still passes (37 learning tests successful).

The dispatcher fixture checks unchanged candidate text/cost/path evidence and
automatic commit after a prior `い` commit. It covers off/long/all configuration
values with no model worker installed in the fixture; it is not a real-model
quality evaluation. It does not claim that the production user intended a
different selection, or that all of the broader original #108 requirements are
complete. No production learning store was read into the fixture or modified.

## Abandoned connection ownership (#102, cases A–D)

Two real private-pipe regressions established RED before correction. A worker
paused after dispatch kept its composition fence after the client timed out and
closed; a replacement connection's `b` was consumed with no preedit. Separately,
a queued `a` was dispatched after its client closed, changing the old `k` reading
to `か`. These are executable counterexamples, not a claim that production logs
uniquely identify these causes.

Accepted connections now expose a generation-bound liveness probe. The fence
retires only claims with positive disconnect evidence. Unknown/busy observations
retain the claim, and a retired claim stays recorded until its owning dispatcher
finalizes, preventing late teardown from arming the Space latch twice. Queries
inspect only claims for the requested host: O(number of that host's claims),
with O(live and retiring sessions) storage and no polling thread or retry loop.
The server checks connection liveness after decoding and immediately before
dispatch; abandoned queued requests terminate without dispatch. A computation
already in progress is not rolled back or forcibly cancelled.

Pipe instances use overlapped handles with explicitly joined operations. This
lets the liveness query run while its owner waits for a read. A Windows client
PID lookup still succeeds after close, and PeekNamedPipe can still succeed on
unread abandoned bytes; neither alone proves liveness. For buffered bytes, a
zero-byte asynchronous write checks the outbound connection without adding a
protocol frame or consuming input. At most one probe operation is retained;
pending probes are polled without waiting and cancelled **and joined** by the
pipe owner before storage/handles are released. Unknown errors remain unknown.

Transport regressions cover a pending server read, an intact waiting client's
exact framed reply and queued input, closed clients with unread bytes, pipe
instance reuse, and observers surviving owner drop. The replacement-connection
regression verifies `b` preedit/commit and that late old-worker finalization does
not rearm Space absorption. The queued-request regression retains `k`.

- `cargo test --locked --offline -p sakura-ipc --lib connection_probe`: 4 passed.
- `cargo test --locked --offline -p sakura-engine --lib disconnected_`: 4 passed.
- `cargo test --workspace --locked --offline`: passed; all owned runners exited.
- `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`: passed.

This does not make a successful pipe write a document acknowledgement, restore
already lost preedit, or reverse learning from an operation already dispatched.
The production host reproduction and installed-build acceptance remain open.

## Installed dictionary and baseline comparison

Read-only dictionary: v1.0.36, 37,383,196 bytes, SHA-256
`ca1b24fc7f3113998fc73e2722a10865ce36f1eae7f39d7e16170a333febb63f`.
The new eight-reading regression passes without further ranking changes:
`つづけようか`, `すすめようか`, `ろうりょく`, `してきますからね`,
`う`, `いて`, `い`, `なに`. The useful candidate must be present and precede
the reported lossy alternative if that alternative is present. This is a
learning-free candidate-order control, not a reproduced production failure.

The first Debug ignored-suite invocation had 32 successes and two failures.
One benchmark explicitly requires Release. The other asserted that a fixture
had zero spanning dictionary paths, while this dictionary has six. A detached
`ef2f021` baseline reproduced that same failure. The candidate-order assertions
passed on both versions. The stale zero-path assertion was removed; frontier
presence, rescoring, and resulting candidate order remain required.

Final Release dictionary suite: **35 passed**, including the new regression and
the existing performance gate (5,000 samples after 500 warmups). Target bridge
p99 was **2.234 ms**, maximum-bound bridge p99 **1.629 ms**, both below the existing
20 ms limit. These are conversion measurements on this machine, not TSF IPC or
end-to-end host-input latency. All owned processes exited.

Command: set `SAKURA_SYSTEM_DIC` to the pinned dictionary, then run
`cargo test --release --locked --offline -p sakura-engine --test shipped_dictionary_ranking -- --ignored --test-threads=1 --nocapture`.

Baseline and modified worktrees must use separate target directories. During
comparison, a shared target reused an old test binary and reported zero matching
tests; that invocation was rejected as evidence. The target-specific local
packages were cleaned and rebuilt before the new test and final Release suite.

Before the passive-poll correction: workspace **1,833 passed / 85 ignored**; Release dictionary
suite **35 passed** separately; Clippy with `-D warnings`, formatting and diff
checks passed. Each invocation ended with zero surviving owned processes.

## Passive candidate polling (#142)

The v1.0.36 `ui-placement` diagnostic category also counted
`PollCandidateCommit` timeouts. The observed 29 records cannot retrospectively
be attributed entirely to geometry updates. New candidate polls use append-only
operation code 11 (`candidate-poll`); historical code 7 keeps its existing name
and count. A mixed-record regression verifies this compatibility.

A second unissued-expiry counterexample was found in passive click polling.
The client returned `Unavailable` before transmitting anything, and TextService
stopped its timer and ended the candidate UI. RED expected `Deferred`, observed
`Unavailable`. On a synchronized connection this now returns `Deferred`; the
existing timer owns the next bounded attempt, with no immediate retry. Missing
or desynchronized connections and actual sent-request timeouts remain
`Unavailable`. A private-pipe timeout control verifies exactly one request and
no hidden retry. The shared callback regression also verifies no wire request,
unchanged session/request identity, and successful following text input.

The distinction and the no-send UI continuation pass their regressions. This
does not establish which of the historical records was a passive poll or prove
that every UI timeout is resolved. Actual host visual acceptance remains open.

Final combined workspace after this correction: **1,836 passed / 85 ignored**,
Clippy `-D warnings`, formatting and diff checks passed; zero surviving owned
processes. The separately measured Release dictionary result above still applies
to the unchanged conversion and learning implementation.
