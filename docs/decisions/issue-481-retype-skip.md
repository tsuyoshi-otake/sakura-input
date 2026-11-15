# Issue #481: Skip erased surfaces on the first retype conversion

## Decision

When the first conversion of a reading retypes the most recently erased
tracked commit, avoid selecting that erased surface and its at most two carried
rejections. This changes the initial selection only; candidate contents and
order stay unchanged, and the user can still select the erased surface.

The session-owned correction trail is read without mutation and without
allocation. A match requires an exactly erased tracked top entry, no erase
overflow, and the same complete reading. Partial erasure, an interrupted
trail, a different reading, non-initial selection, and
`preserve_exact_initial` do not activate the rule.

If the direct candidates include no non-identity surface outside the
rejections, retain the existing selection unchanged. Otherwise, exclude the
rejected surfaces from each existing selection stage: exact learning,
commit cache, general learning, the #448 function-word rule, and the first
non-identity candidate. Candidate indexes and learning strength gates remain
unchanged. If the optional reranker selects a rejected surface, use the
existing preference computed with those exclusions.

The rule does not modify correction learning, tombstones, the trail, or
prediction candidates. A later commit remains subject to #305's existing
correction and staging rules.

## Verification and limits

`learning-replay` compares the shipping engine's choice with and without R1
using the same recorded commits and #305 learning. Fixed and broken use the
final corrected surface as the target. The pre-registered gate requires at
least 10 fixed and at least five fixed per broken. The replay cannot reproduce
the commit cache, reranker, or host-side caret moves, and its input is private.
Scenario tests additionally cover complete and partial erasure, interrupted
and different readings, the no-alternative fallback, candidate navigation,
applied undo, and carried rejections.
