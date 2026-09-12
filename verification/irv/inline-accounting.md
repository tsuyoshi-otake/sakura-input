# Inline test accounting correction (#173)

The original scanner counted every line from the first `#[cfg(test)]` through
EOF. A mid-file test helper therefore classified later production as tests;
even an out-of-line test module declaration counted as an inline body. In the
post-extraction KEY-MODE read set, it reported 4,568 inline lines instead of 30.
The total physical LOC, bytes, file count and regression thresholds never used
that subtraction and remain valid. The historical baseline is preserved.

The corrected structural scanner masks comments and literals, finds exact
`cfg(test)` attributes, follows their item/statement boundaries and counts the
union of covered physical lines. Out-of-line module declarations contribute
zero inline body lines. It handles the forms fixed in
`scripts/fixtures/irv-inline/expected.json`, including mid-file fields, helpers,
statics, impls, nested comments, escaped quotes, raw strings and const generics.
Malformed fixture input fails measurement. This is not a full Rust parser:
`cfg(any(test, ...))` and `cfg_attr` are outside this metric; arbitrary macro
grammar or unfamiliar ambiguous syntax needs a new fixture before support is
claimed. Time and lexical storage are O(N) in source characters.

## Corrected Phase 1 evidence

`phase1-corrected.json` records committed source trees before test separation
(`758c93f`) and after steps 1.1-1.8 (`ed712b4`), scanner SHA-256, each source
manifest SHA-256, historical baseline SHA-256 and snapshot hashes. Measurements
read `git archive` output rather than concurrent working-copy files. Bytes are
the committed archive bytes; Windows checkout line endings can differ.

The only benchmark membership change between these trees adds renderer
`lib.rs` beside `main.rs`, retaining the relocated startup code in the read set.
Issue shapes and semantic estimates are unchanged. These are Phase 1 source
snapshots, not a claim that the historical `97705a5` baseline used this scanner.

| Benchmark | Before physical | Before inline | After physical | After inline | Non-inline delta |
|---|---:|---:|---:|---:|---:|
| KEY-MODE | 22,251 | 12,208 | 10,082 | 30 | +9 |
| CANDIDATE | 27,531 | 14,100 | 13,630 | 190 | +9 |
| TSF-REENTRANCY | 13,859 | 3,959 | 11,610 | 1,706 | +4 |
| TSF-DUAL-KEY | 13,624 | 3,967 | 11,375 | 1,714 | +4 |
| CORE-CONVERSION | 11,044 | 3,764 | 7,511 | 225 | +6 |
| RENDERER-POPUP | 5,939 | 1,908 | 4,581 | 543 | +7 |

For each row, `after physical = before physical - (before inline - after
inline) + non-inline delta`. The small net differences include module/path
declarations, the renderer entry wrapper and formatting. Production/test-body
equivalence and unchanged runtime identities were verified in the extraction
PRs #170-#177. Intentionally retained test helpers still count as inline lines.
Physical reduction does not imply semantic reduction.

The six corrected Phase 1 physical limits round the measured after value up
to the next 100 LOC (a fixed margin of 0-99 LOC). They replace estimates derived
from the defective excluding-tests column, not the final Phase 2-7 design
targets. Desktop evidence and R9 remain separate acceptance conditions.

## Verification

- Verify: `pwsh ./scripts/measure-irv.ps1 -SelfTest`. Expect one PASS line;
  fixed syntax fixtures, missing-entry checks and WARN/FAIL negative controls
  all pass. The mid-file fixture is 15 inline lines; the old algorithm returns
  25. Parent additionally checked same-line union, nested cfg, declaration-only
  modules and a missing final newline.
- Verify: measure both scanners over the same archived after tree. Expect all
  ten physical LOC/byte/file-count triples unchanged; only inline/subtracted
  metrics change. Direct comparison passed.
- Verify: `pwsh ./scripts/measure-irv.ps1 -Compare verification/irv/baseline.json`.
  Expect no critical FAIL and docs within 24,576 bytes. The pre-existing CI
  read-set growth remains WARN; this correction does not rebase it away.
