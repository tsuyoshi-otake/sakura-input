# Context Prediction — Current-State Research

Status: Phase 0 research for Issue #34. This document records the state of the
clean Issue #34 worktree at `8e966df` and does not claim that context prediction
is implemented or enabled.

## Ownership and isolation

- Base commit: `8e966dff456e4e7165e025f97c1f73327ff3f550` (`main`, tracking
  `origin/main`).
- Issue branch: `issue-34-context-prediction`.
- Dedicated worktree: `C:\Users\developer\tmp\sakura-input-issue-34`.
- The repository root was intentionally left untouched. At the start of this
  work it contained tracked edits across dictionary, core preferences,
  engine dispatch/learning/long-conversion/prediction/server/UI, protocol,
  renderer, settings, and TSF files, plus untracked neural-evaluation and
  settings artifacts. A later status check also showed an edit to
  `crates/sakura-engine/src/session.rs`; ownership of all root changes remains
  unknown. No reset, stash, cleanup, or merge was performed.
- No Sakura-Rerank feature branch was visible. The only non-root worktrees
  visible before this branch was created were two detached worktrees at
  `a05e15b`; neither was assigned to this work. Shared-file integration must
  therefore wait for an explicit owner and contract commit.

## Existing prediction path

The current engine already has a bounded, local prediction service, but it has
no context buffer and no neural context task.

| Concern | Current behavior | Evidence |
|---|---|---|
| Candidate sources | System predictive dictionary, user dictionary, and learned prediction history | `crates/sakura-engine/src/prediction.rs`, `PredictionIndex::predict_into`; `LearningService::visit_prediction_history` |
| Internal ranking scratch | `MAX_SUGGESTIONS * 4` (`36`) ranked entries before the final list | `prediction.rs`, `RANKED_SCRATCH` |
| History contribution | Up to `4` history candidates before dictionary candidates | `prediction.rs`, `MAX_HISTORY_SUGGESTIONS` |
| Display limit | At most `9` candidates | `prediction.rs`, `MAX_SUGGESTIONS`; `sakura-proto::CANDIDATE_PAGE_SIZE` |
| Query hand-off | One persistent `sakura-predict` thread and one pending mailbox slot; replacing a pending query increments a coalescing counter | `PredictionRuntime`, `Mailbox::publish` |
| Request terminal states | stopped, empty/oversized, coalesced, or timeout returns no result; callers retain ordinary composition | `PredictionService::request_into`, `Mailbox::wait_into` |
| Engine wait budget | `10 ms` per prediction request | `crates/sakura-engine/src/dispatch.rs`, `PREDICTION_TIMEOUT` |
| Eligibility | Prediction is disabled for sensitive scopes and non-complete/composing states. The current predicate does not independently require `scope_classified == true` or `scope == Normal`; the new context path must add that stricter gate before any context crosses a worker boundary. | `dispatch.rs`, `prediction_is_eligible` |
| Display ordering | Candidate state is cached for the `(session, prediction_generation)` pair; it is not asynchronously replaced after suggestion focus | `dispatch.rs`, `PredictionCache` |

The worker reads a user-dictionary snapshot for each query. Dictionary entries
are indexed once at runtime and prefix lookup is `O(log N + K)` for the system
dictionary; user-dictionary search is bounded by the dictionary's own index.
The current code returns only surface/annotation data to the UI and keeps source
provenance engine-local.

## Session, learning, and privacy boundaries

`Session` is fixed-capacity and currently contains:

- `MAX_SESSIONS = 64` live sessions;
- `prediction_generation` and suggestion focus/selection state;
- an eight-entry volatile commit fingerprint cache (`COMMIT_CACHE_CAPACITY = 8`)
  containing reading/surface hashes and IT-word ratios;
- one grammatical `carry_right_id` across commits;
- no bounded semantic tail of committed text.

The existing learning service provides the useful integration points for a
future context feature:

- `LearningService::learn(reading, surface, left_context, right_context)`;
- `LearningService::generation()` for cache invalidation;
- `visit_prediction_history(prefix, visit)` for local history candidates;
- a fixed `32,768 * 3` slot index, a `20 MiB` log ceiling, `512`-byte history
  text bounds, and a 30-day half-life.

The new context store must not reuse the durable learning record as a raw-text
context store. It must be memory-only, bounded, cleared on session/scope
transitions, and updated only after a definitive Sakura commit. Its admission
condition is a positively classified `Normal` scope. It must reject sensitive,
unknown/unclassified, `test_only`, host-document, clipboard, developer-history,
and other-session text before serialization.

## Existing protocol and worker boundary

- The engine/TSF/renderer protocol is `sakura-proto` protocol version `14`.
  It is a separate UI/input contract and must not be silently extended with a
  worker task.
- The existing `sakura-neural-worker` protocol is a private version-1 frame
  with at most six candidates. Its request contains candidate fingerprints and
  surfaces; the current context byte range is skipped rather than represented
  as a shared contract. It is used by the long-conversion reranker only.
- Issue #34 therefore needs a dormant sibling-task contract rather than an
  immediate production protocol switch. The proposed contract is documented in
  `docs/neural-context-contract-v1.md` and implemented by the std-only,
  non-integrated `sakura-neural-proto` crate. Existing worker and engine
  binaries remain on their current protocols in this phase.

## Baseline measurements

The existing ignored release benchmark was run with:

```text
rtk proxy cargo test -p sakura-engine --release --lib \
  prediction::tests::user_dictionary_prediction_evaluation -- \
  --ignored --nocapture
```

The benchmark uses a small compiled fixture and synthetic user dictionaries;
its numbers are not representative of the full production dictionary or a
host application.

| User entries | Matching entries | Search mean (ns) | Ranking mean (ns) | Worker mean (ns) |
|---:|---:|---:|---:|---:|
| 100 | 9 | 12.6 | 1,265.0 | 3,607.1 |
| 1,000 | 100 | 44.4 | 1,447.2 | 4,857.8 |
| 10,000 | 100 | 50.4 | 1,446.1 | 3,642.2 |
| 10,000 | 10,000 | 8,918.3 | 25,711.0 | 33,466.6 |

The Phase 0 worktree adds an ignored percentile benchmark with 2,000 warm
samples for a 1,000-entry/9-match case. The latest release-optimized run under
local scheduling is recorded here on 2026-08-11; these fixture timings are
intentionally not treated as stable performance claims:

| Path | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|
| In-process ranking | 3.2 µs | 3.3 µs | 3.3 µs | 23.6 µs |
| Mailbox + worker request | 11.8 µs | 20.6 µs | 36.0 µs | 151.1 µs |

These are fixture measurements, not acceptance evidence. There is no current
end-to-end prediction benchmark for the production dictionary, the full
`SendKey` transaction, context append/clear cost, cold/warm model inference,
private working set, or stale-result rate. The Issue #34 quality and runtime
gates remain open.

## Phase 0 conclusions

1. Candidate generation and existing local ranking can remain the source of
   truth. Context prediction should be a sibling score layer over an internal
   top-32 pool while the UI remains top-9.
2. The current single-slot prediction mailbox is a useful local baseline, but
   a shared prediction/rerank broker must not make the 10 ms prediction path
   wait twice or allow a stale result to reorder a focused list.
3. The engine must own the context tail, task semantics, structural priority,
   final ordering, and exact stale validation. A worker may receive one
   immutable bounded snapshot and return residual scores only.
4. The first safe implementation boundary is the dormant shared contract and
   codec. Runtime activation, worker/tokenizer sharing, context-vector caching,
   and installer changes remain blocked on the Sakura-Rerank owner/interface
   decision and the Issue #34 data/runtime gates.

## Verification environment notes

This Windows checkout has `core.autocrlf=true`. Before the repository attributes
were tightened, Git checked manifest-bound JSON/JSONL files and the replay
snapshot out with CRLF even though their committed bytes and SHA-256 manifests
use LF. That made the existing `dictc` release-integrity test and the engine
replay snapshot test fail without any source change. The branch adds LF checkout
attributes for `data/llm-detail-targets/**`, `data/llm-details/**`, and `corpus/**`;
the affected files were normalized in this worktree only and produce no content
diff.

The default parallel workspace run also exposed an unrelated existing test
synchronization issue in `sakura-neural-worker`: the
`simd::tests::request_path_does_not_re_detect_features` counter observed `37`
instead of `35` when other feature-detection tests ran concurrently. The test
passes when isolated, and the full workspace passes with one test thread:

```text
rtk cargo test --workspace -- --test-threads=1
889 passed, 20 ignored
```

No worker source was changed for this issue. The parallel scheduling failure is
kept as a residual verification note rather than being presented as a context
prediction regression.
