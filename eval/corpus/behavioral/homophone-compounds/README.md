# Homophone compound observations

`fixture.tsv` preserves the compound homophones submitted for Issue #79.
The 158 `candidate_observation` rows describe plausible competing surfaces;
they are not unconditional Top-1 gold labels and do not tune dictionary costs.
Their three context/Top-1 columns use `-` as an explicit unavailable marker.
The two `context_required` rows deliberately share the same reading and right
context while changing the left context. They document cases that cannot be
scored correctly from the local reading alone.

The fixture is separate from the fixed 50-case Stage 1 quality corpus. It is a
versioned review/evaluation asset for future context-aware capture. Candidate
details remain exact-entry data: a multi-segment surface assembled from these
words must not inherit the detail of any component word.
