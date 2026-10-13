# Issue #309: repeated-key session history

## Scope and acceptance

This change follows the "excludable by analysis" acceptance route. Real keys
already store `repeat`, and `history mine` rejects each commit interval containing
a repeat. Sessions without commits produce no candidates. Input behavior,
session allocation, privacy exclusions, storage v2 and protocol v24 are unchanged.

The reported history shape and one `CancelledStateTerminalized` disconnect per
held key in the logged window, on one host thread, narrow the investigation to
the shared cancellation terminal. They do not identify its upstream caller.

## Cancellation path

`terminalize_cancelled_state(true)` clears the UI lease, schedules candidate
teardown and calls `disconnect(CancelledStateTerminalized)`. That retires the
engine link when its borrow succeeds; the next connection creates a new session.
The six call sites are:

| Caller | Condition |
| --- | --- |
| `promote_pending_focus_gain_reconciliation` | Acquires reconciliation ownership |
| `dispatch_focus_reconciliation` | Repeats terminalization after abandonment |
| `defer_refused_focus_reconciliation` | Transfers retirement to a bounded retry |
| `abandon_composition_projection` | No cancelled completions remain |
| `settle_cancelled_writes_inner` | Engine-side focus commit already started |
| `settle_cancelled_writes_inner` | Nonempty completions with reset requested |

Candidate A: an accepted asynchronous composition-start write remains pending;
the next real key observes another context identity, cancels that old write and
its dependent tail, and reaches the shared terminal before applying the key in
a fresh session. The nonempty completion list skips the additional empty-context
reset reasons. A delayed callback alone, in the same context, does not cancel it.

Candidate B: synchronous and asynchronous write requests are refused before the
callback runs. `reject_requested_write` produces a rejected completion and the
same terminal within that key callback. The request owner returns success after
settlement, so the outer key path adds no predecessor-failure disconnect reason.

The available store and disconnect evidence contains neither the completion
cause nor the edit-request result or canonical context identity. Both candidates
fit; an IME identity bug versus legitimate host churn/refusal remains unresolved.

## Source comparison

| Revision | `text_service.rs` blob |
| --- | --- |
| v2.0.3 | `6b88e0ffa1113c16d9b395dd28d2942a0608d3f2` |
| v2.0.7 | `6b88e0ffa1113c16d9b395dd28d2942a0608d3f2` |
| Investigated HEAD, `44bb75c792c40a214cecb57b0930b1b8dc2e31f7` | `3ca009465f269361c11da1498d9315f26d68d1ef` |

The HEAD difference is AI-result construction in commit `9e09970` (#271), not
cancellation behavior. The edit-session, journal and engine-link files are
identical across these revisions; this path has not changed in the comparison.

## Regression and residual risks

`mine_single_key_repeat_sessions_do_not_taint_later_clean_commit` uses synthetic
single-key sessions with idle-to-composing repeats, then ordinary committed
input. Commit-bearing and mixed-session variants assert repeat-interval rejection
and unchanged clean candidates, including typing and frequency metadata.
`developer_history_captures_real_keys_but_not_test_unclassified_or_sensitive_keys`
now covers stored real repeats and repeat-key privacy/probe exclusions.

Verified 2026-10-13: both tests pass. Removing the production `|| key.repeat`
guard in `reconstruct_typing` fails the mining test at candidate equality (an
extra repeat-derived candidate), and the restored source passes after a
confirmed rebuild. These tests do not establish the actual host lifecycle cause
or reduce stored volume. Old records remain untouched. Compaction can remove
earlier repeat evidence, and the full disconnect log prevents newer
observations; #313 tracks diagnostics retention and the trigger detail.
