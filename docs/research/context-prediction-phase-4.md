# Context Prediction — Phase 4 Snapshot and Evaluator Evidence

Status: dormant top-32 snapshot and offline replay evaluator for Issue #34. No
production caller, replay corpus score, neural inference, shadow-mode capture,
setting, installer payload, or visible candidate reorder is claimed here.

## Production boundary

The existing `prediction::PredictionResult`, mailbox, and worker remain a
fixed nine-candidate production path. Phase 4 adds independent
`prediction_snapshot` and `context_evaluation` modules; neither is called by
`prediction.rs`, `dispatch.rs`, `session.rs`, the TSF, or a worker. A regression
test fixes both boundaries explicitly:

- internal dormant snapshot: at most 32 candidates;
- existing visible suggestion page: exactly 9 candidates.

This avoids changing Issue #24-owned integration files or interpreting a test
fixture as permission to activate context prediction.

## Snapshot contract

`PredictionSnapshot::build` accepts transient engine-canonical reading/surface
strings and retains only fixed-size fingerprints and candidate metadata. It does
not apply NFKC, width conversion, or a second text normalization pass. Future
production integration must pass the exact canonical strings owned by the
candidate generator so identity cannot diverge from commit behavior.

The immutable snapshot contains:

- nonzero session, context-generation, and composition-generation correlation;
- an order- and cost-sensitive 32-byte candidate-set fingerprint;
- deterministic nonzero candidate ids;
- reading/surface hash and UTF-8 byte length, not raw text;
- base cost, authority, source, right-id, IT flag, and original index;
- exact system/user dictionary ordinal when the producer owns one.

Deduplication uses the canonical `(reading, surface, dictionary identity)` key.
Candidates with the same text but different exact dictionary ordinals remain
distinct, which preserves homographs and dictionary-detail identity. Identical
keys collapse deterministically to their first occurrence. The builder rejects
oversized pools rather than truncating a protected candidate.

Only positively classified `InputScope::Normal`, non-`test_only` input is
admitted. Sensitive and unclassified scopes, zero correlation values, empty
text, oversized text, candidate-id collisions, and pools above 32 fail before a
partial snapshot escapes. Snapshot `Debug` output is hash/metadata-only.

The fingerprint is a correlation checksum, not a cryptographic artifact hash.
A response is current only when session, context generation, composition
generation, and the complete candidate-set fingerprint all match exactly.

## Offline replay evaluator

`evaluate_replay` consumes ordered hash-only observations and reports the
following metrics with their raw numerator/denominator:

| Metric | Definition in this evaluator |
|---|---|
| Oracle Recall@9/16/32 | Ground-truth candidate exists in the generator's original pool prefix |
| Top-1/3/9 | Ground-truth rank in the effective proposed order |
| MRR | Mean reciprocal rank, with a missing candidate contributing zero |
| NDCG | Single-relevant-item discounted gain, with a missing candidate contributing zero |
| KSR | Aggregate `(keystrokes without prediction - with prediction) / without prediction` |
| Persistence | Previous top-1 remains anywhere in the next top-9 for the same session |
| Churn | Fraction of the previous top-9 membership absent from the next top-9 |
| Stale | Exact-correlation failures per observed response |
| Duplicate | Canonical duplicates removed per offered snapshot candidate |
| Source hit | Top-9 hits and oracle opportunities split by history/system/user source |

Stale proposed rankings are counted but evaluated as the unchanged generator
order. This models fail-closed runtime behavior instead of crediting a result
that must not be applied. Persistence and churn exclude stale transitions and
cross-session transitions.

Before scoring, the evaluator rejects duplicate or unknown ranked ids, more than
32 ids, impossible keystroke counts, protected-candidate loss, protected-order
changes, and an ordinary candidate placed ahead of the protected structural
tier. Exact-learning and user-dictionary candidates therefore cannot be demoted
by an offline result that the evaluator accepts.

## Evidence and remaining gates

Unit fixtures cover privacy admission, deduplication, homograph identity,
candidate-set fingerprint changes, every stale correlation dimension,
fail-closed protected tiers, metric aggregation, and stale fallback. The global
allocation counter observes zero heap allocations while building a snapshot and
evaluating one observation.

No checked-in real replay corpus exists yet, so this phase deliberately reports
no quality, KSR, persistence, or churn acceptance value. The next data gate must
define a consented, privacy-reviewed replay export with stable candidate ids and
frozen train/tuning/held-out partitions before model or shadow-mode conclusions
are drawn.

## Verification rubric

- `Verify:` snapshot unit and allocation tests. `Expect:` maximum 32, visible
  maximum 9, canonical deduplication, raw-text-free retained state, exact stale
  rejection, privacy fail-closed, and zero allocations.
- `Verify:` evaluator unit tests. `Expect:` all named Phase 4 metrics expose
  sample counts; stale output preserves generator order; protected candidates
  cannot be lost, reordered, or demoted.
- `Verify:` `rtk cargo fmt --all -- --check`, engine tests, workspace tests,
  workspace clippy with `-D warnings`, dependency policy, and
  `rtk git diff --check`. `Expect:` all pass and no cargo/rustc/test process for
  this worktree remains.
