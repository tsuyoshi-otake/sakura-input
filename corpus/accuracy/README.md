# corpus/accuracy/

The accuracy gate (#333): authored sentences and words measured against a built
dictionary, compared row by row with a committed snapshot, so a change that
makes some rows worse is visible even when the totals look fine. The tool is
`accuracy-gate` in `tools/conversion-eval`. `corpus/held-out.tsv` and
`corpus-eval` are a different, frozen release gate and are not replaced.

| File | Role | Edited by |
| --- | --- | --- |
| `conversion.tsv` | 1,566 authored sentences: `id, slice, reading, expected, variants` (`\|`-separated) | hand |
| `prediction.tsv` | 1,605 authored words: `id, reading, surface, slice, pos` | hand |
| `conversion.snapshot.tsv` | per row: `id, state, top` (state 2 = expected, 1 = variant, 0 = wrong) | `--accept` |
| `prediction.snapshot.tsv` | per word: `id, full, half, k9, k1` (ranks inside the nine suggestions; 0 = absent) | `--accept` |
| `floors.tsv` | integer floors per slice and `ALL` | `--accept` |
| `accepted-breaks.tsv` | rows allowed to get worse: `kind, id, issue, reason, date` | hand |
| `external/README.md` | the local, uncommitted Tatoeba and Mozc layer | hand |

The eight slices are `chat`, `email`, `general`, `grammar`, `homophone`, `it`,
`loanword` and `numbers`. All files are LF (`.gitattributes`: `/corpus/**`).
`CODEOWNERS` assigns this directory to the owner.

## What is measured

- Conversion: `Converter::new()` with `ConversionOptions::default()` (768
  candidates), no learning, no reranker, no context, the top candidate only.
  This is the first conversion of a new session. Lenient = the top is
  `expected` or a variant; strict = the top is `expected`.
- Prediction: the reading is typed one character at a time through
  `PredictionRuntime` (IT ratio 0, no learning) and the rank of the surface is
  read inside the nine suggestions after each character. `full@1` and `full@9`
  look at the whole reading; `half@1` and `half@9` look after `ceil(n / 2)`
  characters. `save9` is the sum of `n - k9` over all words, where `k9` is the
  first prefix length whose rank is 1 to 9 (0 saved if never); `save1` is the
  same with rank 1. They count keystrokes before the choice key and say nothing
  about how long a person takes to find the word: they are regression
  indicators, not the experienced saving.
- Learning, the reranker and context are not measured. A regression there needs
  its own measurement.

## Corpus rules

The loader rejects a corpus that would hide or inflate a result:

- a variant equal to the reading (a kana answer for a conversion that did not
  happen), an empty variant;
- a duplicate id, a duplicate reading in `conversion.tsv`, a duplicate
  reading and surface in `prediction.tsv`;
- an unknown slice, a wrong column count, a wrong header.

Do not commit rows taken from Tatoeba or the Mozc evaluation set, URLs, mail
addresses, telephone numbers or anyone's input history. A new improvement should
add five or more new rows that are not the rows it fixed, and update the
snapshot in the same PR.

## Run it

1. Build the dictionary exactly as CI does, with the pinned Mozc checkout
   (see `data/README.md` and `scripts/build-dictionary.ps1`):

   ```powershell
   ./scripts/build-dictionary.ps1 -MozcSource <mozc checkout> -GlossarySource third_party/smile-chat-public -OutputDirectory artifacts/accuracy -SkipDeterminismCheck
   ```

   The gate measures the shipped dictionary. The report header shows the dictionary's SHA-256 and whether it
   equals the `system.dic` hash in `data/dictionary-build.report.json`.

2. Run the gate (PowerShell 7):

   ```powershell
   $env:CARGO_HTTP_CHECK_REVOKE = 'false'
   cargo run --release --locked -p sakura-conversion-eval --features accuracy-gate --bin accuracy-gate -- --dictionary artifacts/accuracy/system.dic --check
   ```

   Without `--check` or `--accept` it only measures and reports. Other
   options: `--report <file>` keeps a copy of the report, `--corpus-dir`
   and `--build-report` override the default locations.

## Read the report

- Header: dictionary and corpus SHA-256, the options (`max_candidates = 768`).
- Two tables, one row per slice and `ALL`, with `before->after (+n)` counts
  against the committed snapshot. A count that did not change prints once.
- Conversion fixed and broken rows (state changed), and the rows whose top
  changed with the same state. The last group is report only.
- Prediction improved and worsened words, rank moves inside the nine (report
  only), and the words that entered or left the nine after the full and half
  reading.
- `check: PASS`, or `check: FAIL` with the violated conditions below.

## Failure conditions of `--check`

1. A row's state changed while the snapshot was not updated (conversion: state
   2/1/0; prediction: `full@1`, `full@9`, `half@9` or `k9`).
2. A row got worse and `accepted-breaks.tsv` does not list it.
3. A measured count is below its floor.
4. A floor is below the count the snapshot files imply (this stops lowering
   only the floors).
5. An accepted break names an unknown id, or a row that is now at its best
   state (conversion state 2; prediction `full@1`, `half@9` and `k9 = 1`).

A changed top surface with the same state and a reordering inside the nine
suggestions are not failures.

## Accept a change

1. Run the gate and read the report: every changed row is listed.
2. For each worsened row, add one line to `accepted-breaks.tsv` with the kind
   (`conversion` or `prediction`), the row id, the Issue (`#123`), a one-line
   reason that states the Issue's measured fixed and broken rows, and the date
   (`YYYY-MM-DD`).
3. Run the same command with `--accept` instead of `--check`. It refuses while
   a worsened row has no entry; otherwise it rewrites the two snapshots and
   sets every floor to the newly measured count.
4. Remove accepted-break entries whose row is back at its best state (condition
   5 reports them), commit the changed files and paste the report into the PR.

Any PR that changes the dictionary or a cost updates the snapshot in the same
commit. The owner reviews the diff of this directory; the `main` ruleset needs
no approval, so the machine checks above and that review are the gate.

## Time

Measured on the development PC (release build, warm cargo cache, the 2.0.16
dictionary): conversion 0.2 s (rows run on up to eight threads), prediction
about 18 s (single prediction worker, sequential), so the gate body is about
18 s against a budget of 60 s; building `accuracy-gate` took 18 s with
dependencies cached. The dictionary build is the long step: 67 s locally (dev
profile) and estimated at about 90 s on a runner (half of the two builds
`installer.yml` does; not measured). The CI total is estimated at 5 to 8
minutes and has not been measured on a runner yet.

## CI

`.github/workflows/accuracy-gate.yml` runs on every pull request (no path
filter) and on demand. Its job `accuracy-gate` is a required status check in
`ci/rulesets/main.json`. It checks out the pinned Mozc data, builds the
dictionary with `-SkipDeterminismCheck` (`installer.yml` already proves the
build is deterministic), runs `accuracy-gate --check`, writes the report to the
job summary and uploads it as the `accuracy-report` artifact.
