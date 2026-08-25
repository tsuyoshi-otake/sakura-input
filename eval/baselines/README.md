Baselines are captured candidate lists plus artifact identity, not git branches.
A baseline file that cannot name its evaluator or engine SHA-256 and its
dictionary SHA-256 is invalid.

Issue #93's cross-version candidate snapshots are under
`ranking-comparison-issue93/`. Their evaluator SHA-256 identifies the standalone
tag-linked capture artifact; it must not be mislabeled as a shipped engine hash.
