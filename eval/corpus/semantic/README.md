# Semantic evaluation corpus

Cases in this tree are the Judge's input *after* capture. They must not contain
raw developer history. Expected surfaces, issue numbers, and `constraints`
exist for the deterministic oracle and for humans maintaining the corpus.
The runner strips all of that before Codex sees a case.

Issue #66 is the first vertical slice: literal ASCII tokens, mixed unresolved
Latin, typo recovery, and normal-Japanese negative controls.

Each case may provide `input.typing`, the exact character sequence used by the
real-engine capture runner. It is capture metadata, not a Judge signal:
`typing`, expected surfaces, issue references, and `constraints` are excluded
from the blinded prompt. Cases without an explicit typing sequence fail closed
in real capture mode instead of silently treating a reading as keystrokes.

`history-derived/` contains only explicitly approved, minimized review
projections. Each case uses an opaque `hist-` identity, bounded reading and
typing, empty-by-default context, and `privacy_provenance` set to
`local-opt-in-normal-commit-v1`. DPAPI frames, surfaces, session IDs,
timestamps, process names, and review TSV files stay outside the repository.
The committed `history-derived/approved-case-ids.txt` contains only opaque
approval identities.
The semantic manifest records the derived count, generation rule, and source
hash so regeneration is deterministic without publishing the source history.
