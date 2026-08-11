# Sakura Context Intelligence Contract v1

Status: dormant contract draft for Issue #34. The corresponding Rust types and
codec are in `sakura-neural-proto`; no production engine, renderer, TSF, or
worker binary uses this contract yet.

## Purpose and non-goals

Context Prediction and Sakura-Rerank are sibling tasks over one versioned
snapshot and score contract. They may share tokenization, an encoder, a worker,
or an inference broker later, but sharing model weights is a measured decision.
The default design keeps task-specific checkpoints if multi-task training causes
negative transfer.

This contract does not replace candidate generation. The engine continues to
own the system dictionary, user dictionary, learned-history candidates,
lattice/Viterbi conversion, structural tiers, exact learning, and final order.
The model contributes only a bounded residual score to an existing candidate.

## Task limits

| Task | Input pool | UI/result role | Deadline budget |
|---|---:|---|---:|
| `Prediction` | at most 32 candidates | engine still displays at most 9 | at most 10 ms |
| `Rerank` | at most 6 full-path candidates | existing conversion list semantics | at most 500 ms |

The deadline is a request-side upper bound for the broker/worker. It is not a
claim that inference meets the bound. A prediction transaction must not wait
for the same work once in the engine and again in the worker. The broker owns
one bounded latest-wins lane per task and reports a terminal fallback when the
deadline or process boundary is reached.

## Snapshot ownership and privacy

The engine constructs one immutable `ContextSnapshot` after it has validated
all of the following:

- the input scope is positively classified `Normal`;
- the context text was committed by Sakura in the same session;
- the candidate pool is complete and frozen for this request;
- the request is not `test_only` and is not a reconversion or direct-input
  operation;
- the session, context generation, composition generation, candidate-set
  fingerprint, model fingerprint, and tokenizer fingerprint are known.

The snapshot contains only a bounded semantic tail of Sakura-committed text,
the current reading/prefix, and candidate features required for scoring. It
never contains raw key events, uncommitted composition, host-document text,
clipboard text, developer input history, another session/application's text,
Password/URL/Email/Digits content, or Unclassified/unknown input.

The context tail is memory-only and is cleared on session deletion, scope
change to sensitive/unknown, deactivation/context replacement, explicit
context clear, and any state where the engine cannot prove ownership. The
worker receives no user dictionary or learning store and has no authority to
persist or infer additional context.

## Candidate and score semantics

Each input candidate carries:

- an engine-owned opaque `candidate_id`;
- the existing signed base cost;
- a source authority (`Ordinary`, `ExactLearning`, or `UserDictionary`);
- the candidate surface needed by the model, bounded by UTF-8 byte length.

The worker returns `(candidate_id, residual)` pairs only. The residual is a
bounded signed integer and is added to the existing base score by the engine;
it never replaces the base score, emits a new candidate, or returns an order.
The engine clamps/ignores residuals for exact-learning and explicit
user-dictionary candidates according to their existing priority rules.

Responses must contain every input candidate exactly once, with no unknown or
duplicate ID. Missing, extra, duplicate, non-finite/out-of-range, or otherwise
malformed scores are a fail-closed response. A response with a different task,
session, generation, candidate-set fingerprint, model fingerprint, or tokenizer
fingerprint is stale and is discarded without changing the local order.

Once a candidate list is displayed or suggestion focus is entered, no later
neural result may reorder that visible list. The engine may use a result only
while the exact snapshot remains current.

## Wire contract

The dormant codec uses a bounded little-endian frame:

```text
u32 payload_length
u32 magic = "SCV1"
u16 wire_version = 2
u8  message_kind (request=1, response=2)
u8  task_kind
u64 request_id
u64 owner_id
u64 session_id
u64 context_generation
u64 composition_generation
[u8; 32] candidate_set_fingerprint
[u8; 32] model_fingerprint
[u8; 32] tokenizer_fingerprint
...
```

The complete frame is capped at 32 KiB and every length/count is checked before
allocation. Request fields include the normal-scope marker, test-only marker,
deadline, committed-context bytes, current reading bytes, and bounded
candidates. Response fields include an explicit `Ready` or `Unavailable`
terminal status and residual pairs. Unknown versions, message kinds, task
kinds, enum values, trailing bytes, invalid UTF-8, duplicate IDs, and all bound
violations are rejected; no decoder path panics or guesses at a newer layout.

The existing `sakura-proto` engine/TSF protocol v14 and the current private
Sakura-Rerank worker protocol v1 are unchanged. Activation of this contract is
a separate integration change requiring an agreed shared-file owner, protocol
goldens on both sides, privacy tests, stale-result tests, and an explicit
rollout decision.

## Acceptance checklist for activation

- [ ] Candidate pool and structural priority are proven unchanged by replay.
- [ ] Context append/clear occurs only at definitive commit transitions.
- [ ] Sensitive, unknown, unclassified, and `test_only` payload count is zero.
- [ ] Protocol golden frames and generated malformed frames are deterministic.
- [ ] Session/generation/fingerprint mismatch has zero applied stale results.
- [ ] Prediction p99 and end-to-end allocation gates pass on the named runner.
- [ ] Model quality, harm/stability, user-dictionary, and exact-learning gates
      pass with recorded confidence intervals.
- [ ] Worker failure, timeout, crash, or artifact mismatch preserves local
      ranking and does not block input.
- [ ] Production defaults, settings UI, and installer payload receive separate
      approval after the gates above.
