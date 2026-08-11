# Context Prediction — Phase 2A Engine Core Evidence

Status: dormant engine core and offline-only local baseline for Issue #34. This
document does not claim production context prediction, neural inference, or a
change to visible candidate order.

## Ownership boundary

Issue #24 has a concrete reranker boundary, but its current `session.rs` and
`dispatch.rs` integration remains part of an intentionally dirty root worktree,
not an agreed shared contract commit. Phase 2A therefore does not edit those
files. It adds `context_intelligence.rs` as an independent engine module and
depends only on the std-only `sakura-neural-proto` contract plus Sakura's
existing fixed-capacity protocol types.

The module is compiled and tested, but it has no production caller. A later
integration commit must connect it only at definitive engine commit terminals
and explicit session/scope/deactivate/context-replacement lifecycle boundaries
after the shared-file owner and base commit are fixed.

## Semantic context lifecycle

`SessionSemanticContext` is an allocation-free, inline 256-byte UTF-8 tail with:

- a nonzero monotonic context generation;
- a sentence generation;
- a commit count saturated at eight;
- a deterministic 32-byte correlation fingerprint;
- an allocation-free immutable snapshot;
- redacted `Debug` output that reports byte count and correlation metadata but
  never the retained text.

Only a definitive, non-`test_only` commit in a positively classified
`InputScope::Normal` context may append text. Appending keeps the newest complete
UTF-8 scalars and never splits a code point. Empty input is rejected. Sensitive
or unclassified scope clears the tail. Explicit clear reasons cover session
deletion, context replacement, deactivation, ownership loss, and user-requested
clear. Every clear advances both generations even if the tail is already empty,
so a lifecycle boundary always revokes an older snapshot.

The fingerprint is a deterministic correlation checksum over the bounded bytes
and generations; it is not an artifact-integrity or cryptographic identity.
Exact runtime admission still requires the session, generations, candidate/model/
tokenizer fingerprints, and privacy checks in the shared contract.

## Storage and allocation decision

Measured on the Windows x64 debug test target:

| Type | Bytes |
|---|---:|
| Current `Session` before Phase 2 integration | 8,520 |
| `SessionSemanticContext` | 288 |
| Immutable semantic snapshot | 320 |
| Projected `Session` with one inline context | 8,808 |

An inline context would add 288 bytes per live session (3.38% of the current
type), or 18,432 bytes at the existing 64-session cap. `SessionTable` already
owns its slots in one heap allocation, so inline storage adds no allocation or
pointer ownership to the key path. The allocation counter observed zero heap
allocations across append, truncation, snapshot, and clear operations.

This supports inline storage when shared-file integration is approved. The
production engine working set is unchanged in Phase 2A because no live `Session`
contains the dormant core yet.

## Fixture append timing

The ignored release microbenchmark ran 1,000 warmups and 10,000 measured
UTF-8 appends on the local Windows runner:

| p50 | p95 | p99 | max |
|---:|---:|---:|---:|
| 100 ns | 200 ns | 200 ns | 465.2 us |

This is a narrow `Instant`-based fixture measurement, not an end-to-end key-path
or acceptance value. The isolated maximum is retained as scheduler/noise
evidence rather than removed. Production admission still requires the named
runner, full dispatch integration, working-set measurement, and the Issue #34
runtime gates.

## Offline non-neural baseline

`rank_local_baseline` evaluates existing engine-owned signals over at most 32
candidate fingerprints:

- previous grammatical `right_id`;
- the bounded IT-domain ratio;
- a volatile recent exact surface fingerprint;
- existing base cost and shared candidate authority.

The replay input contains no raw surface text. Exact-learning and explicit
user-dictionary candidates remain in a protected structural tier and receive no
residual. Ordinary candidates receive bounded bonus-only residuals; original
index is the deterministic final tie-breaker. Duplicate IDs and pools above 32
fail closed before producing a partial ranking.

This baseline is intentionally not called by production `prediction.rs` and does
not change the displayed top nine. The next independent step is replay/evaluator
tooling and a bounded top-32 snapshot. Production ranking integration remains a
later gated decision.

## Verification rubric

- `Verify:` context lifecycle unit tests. `Expect:` classified Normal commits
  append; sensitive/unclassified and explicit lifecycle transitions clear;
  `test_only` is pure; UTF-8 remains valid; older snapshots are revoked.
- `Verify:` redacted debug and allocation tests. `Expect:` raw context is absent
  from debug output and append/snapshot/clear allocate zero heap objects.
- `Verify:` deterministic local baseline tests. `Expect:` protected authorities
  cannot be demoted, existing signals affect only ordinary candidates, and
  duplicate/oversized pools fail closed.
- `Verify:` targeted engine tests, clippy with `-D warnings`, formatting,
  workspace tests, and `git diff --check`. `Expect:` all pass and no repository
  test process remains.
