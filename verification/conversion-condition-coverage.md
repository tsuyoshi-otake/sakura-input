# Conversion correction condition evidence

Scope: six explicitly listed production decisions. MCC and unique-cause MC/DC are witnessed by real-operation fixtures, with both sides of every corresponding atomic branch confirmed by LLVM branch counters. This is not compiler-generated MC/DC or whole-workspace MC/DC coverage.

| Decision (condition order) | MCC | MC/DC | Independent pairs | Atomic branch outcomes | LF-normalized source SHA-256 |
|---|---:|---:|---|---:|---|
| core.single_kanji_guard (full, table absent) | 4/4 | 2/2 | 00/10, 00/01 | 4/4 | 7a5bdd3666c5d2b3128b0058fcced61c683f29d42082b5c56b653090c93b5f11 |
| projection.tail_overflow (tail allowed, prefix exists) | 4/4 | 2/2 | 01/11, 10/11 | 4/4 | 82b649a717bb06bec895c47f2f1c98664f0289f27c97d5000317531a0e4d2888 |
| history.planned_mismatch (backup mismatch, replacement mismatch) | 4/4 | 2/2 | 00/10, 00/01 | 4/4 | 395d9827b34dd715dfd873d3d48f87f4c2e38b61332bce9713d54e625e87e7e7 |
| history.legacy_mismatch (backup differs, replacement differs) | 4/4 | 2/2 | 00/10, 00/01 | 4/4 | 395d9827b34dd715dfd873d3d48f87f4c2e38b61332bce9713d54e625e87e7e7 |
| history.canonical_generation (matches old, matches new) | 4/4 | 2/2 | 00/10, 00/01 | 4/4 | 395d9827b34dd715dfd873d3d48f87f4c2e38b61332bce9713d54e625e87e7e7 |
| history.plan_shape (wrong length, wrong magic) | 4/4 | 2/2 | 00/10, 00/01 | 4/4 | 395d9827b34dd715dfd873d3d48f87f4c2e38b61332bce9713d54e625e87e7e7 |

The fixtures assert concrete candidate contents, overflow behavior, exact recovery errors, and byte-preservation/cleanup outcomes before emitting each vector. Short-circuited operands have fixture-established values; LLVM counters separately prove each operand was actually evaluated both true and false in the instrumented suite. No skipped operand is counted as an executed LLVM branch.

For OR decisions the pairs are 00/10 and 00/01; for AND decisions they are 01/11 and 10/11. Each pair changes exactly one condition and changes the observed decision outcome. The checker rejects missing combinations, duplicate vectors, or a condition with no such pair.

Multiple instrumented test binaries can report the same atomic source range. Their counters are summed by that exact range; distinct operands remain distinct. The checker also requires both outcomes for each operand, rather than accepting a test executable exit code as coverage.
