# Issue #481: Skip erased surfaces on the first retype conversion

## Decision

When the first conversion of a reading retypes the most recently erased
tracked commit, avoid selecting that erased surface and its at most two carried
rejections. This changes the initial selection only; candidate contents and
order stay unchanged, and the user can still select the erased surface.

The session-owned correction trail is read without mutation and without
allocation. A match requires exactly one pending erased commit, an exactly
erased tracked top entry, no erase overflow, and the same complete reading.
Multiple erased commits, partial erasure, an interrupted
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
Reconversion preview clears the trail on its cloned session, just as actual
reconversion clears it on the live session.

## Verification and limits

`learning-replay` compares the shipping engine's choice with and without R1
using the same recorded commits and #305 learning. Fixed and broken use the
final corrected surface as the target. The pre-registered gate requires at
least 10 fixed and at least five fixed per broken. The replay cannot reproduce
the commit cache, reranker, or host-side caret moves, and its input is private.
R1 is compared only for conversions started by `convert`; kana cycling does not
use the initial selection path. Narrowing the pending-stack condition was
decided after examining the data, not before the original gate was registered.
Scenario tests additionally cover complete and partial erasure, interrupted
and different readings, the no-alternative fallback, candidate navigation,
applied undo, and carried rejections.

## Evidence (counts only)

The developer input history is private, so this section records counts only.

Real-history split: the trail's rules were mirrored over the recorded keys from
2026-11-05 to 2026-11-15. The reference is the first candidate actually shown.

| Pending erased commits | Fixed | Broken | Still wrong | First half (fixed / broken) | Second half (fixed / broken) |
| --- | ---: | ---: | ---: | --- | --- |
| Exactly one | 23 | 1 | 12 | 11 / 1 | 12 / 0 |
| Two or more | 6 | 4 | 0 | 4 / 2 | 2 / 2 |

The halves split at the median conversion time. Weighting one broken as five
fixed, exactly one pending commit scores +18 and two or more score -14, so the
two-or-more case fails the gate on its own (6 < 5 × 4). Four of the five replay
breaks before the narrowing erased two or more commits, retyped from the
leftmost reading, and reselected the same surface. The fifth was a kana-cycle
conversion, which the product never routes through this rule.

G1 replay (`learning-replay`, same private input): 25 fixed / 5 broken before
the narrowing, 20 fixed / 0 broken after it. The run after the narrowing covers
8,795 conversions, changes 30 initial selections, and lowers the replay's
incorrect first candidates from 526 to 506. Two runs gave identical output.

Alternatives measured on the same data and not adopted: lowering a corrected
surface for the rest of the session (1 fixed, 2 broken), and learning a
hiragana selection (3 fixed, 4 broken).
