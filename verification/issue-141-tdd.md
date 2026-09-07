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
