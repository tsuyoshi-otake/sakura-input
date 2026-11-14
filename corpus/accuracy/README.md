# corpus/accuracy/

The accuracy gate (#333): authored sentences and words measured against a built
dictionary, compared row by row with a committed snapshot, so a change that
makes some rows worse is visible even when the totals look fine. The tool is
`accuracy-gate` in `tools/conversion-eval`. `corpus/held-out.tsv` and
`corpus-eval` are a different, frozen release gate and are not replaced.

| File | Role | Edited by |
| --- | --- | --- |
| `conversion.tsv` | 1,587 authored sentences: `id, slice, reading, expected, variants` (`\|`-separated) | hand |
| `prediction.tsv` | 1,605 authored words: `id, reading, surface, slice, pos` | hand |
| `conversion.snapshot.tsv` | per row: `id, state, top` (state 2 = expected, 1 = variant, 0 = wrong) | `--accept` |
| `prediction.snapshot.tsv` | per word: `id, full, half, k9, k1` (ranks inside the nine suggestions; 0 = absent) | `--accept` |
| `context.tsv` | 105 homophone pairs (210 rows): `id, pair, oneshot_reading, oneshot_expected, clue_reading, clue_surface, reading, expected, variants` | hand |
| `context.snapshot.tsv` | per row: `id, oneshot state/top, context state/top, bridge` | `--accept` |
| `floors.tsv` | integer floors per slice and `ALL` | `--accept` |
| `context-floors.tsv` | integer floors for the context counts | `--accept` |
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
- Context (#471, phase 1): each row converts `oneshot_reading` in a fresh
  session, then replays a commit of `clue_surface` for `clue_reading` and
  converts `reading` through the same carry the production Apply path stores —
  the committed candidate's last-segment `initial_right_id` plus a
  `CrossCommitBridge` derived by `cross_commit_tail` inside
  `SessionCrossCommitBridge`'s bounds. `bridge` in the snapshot means the clue
  produced a usable tail; `context cross-commit passes` counts runs of the
  bounded cross-commit pass. Both forms keep `expected`/`variants` scoring.
  Pair buckets count a pair's two rows under committed context: both expected,
  exactly one, or neither. The measurement is read-only for ranking — it asks
  whether context *would* have helped, it does not change what the engine
  produces.
- Learning and the reranker are not measured. A regression there needs its own
  measurement.

## The context corpus and its seeds (#471)

`context.tsv` starts from the 50 homophone pairs listed in Issue #471 (pairs
p001–p050, in issue order) and adds 55 more pairs the same way (p051–p105).
The seeds are full phrases; the corpus separates each phrase into a clue that
is committed first and a target reading converted after. Three rewrite rules
were applied wherever a seed did not fit that shape:

- The clue must precede the target, so a seed whose homophone word sits at the
  start of the phrase was reworded (`あきじかん`/`あきのそら` became the pair
  `あき`: 空き/秋, with ひまな→暇な and 紅葉→紅葉の as the clues). Where the
  seed's homophone is only part of a longer word (`試作機` vs `施策`), the
  target is the shared reading (`しさく`), not the whole word.
- Both rows of a pair must read the same and expect different surfaces —
  the loader rejects any other shape, and rows the review found not to be
  homophones (`ぶしょ`/`ばしょ`, readings that carry a different vowel) were
  replaced (`いどう`: 異動/移動 stands for the seed 部署を異動する/場所を移動する).
- `expected` must be producible from `reading`; misread surfaces (解析 for
  かいけい, 大王 for たいおう, and similar) were replaced by real homophones
  of the same reading or by a new pair.

All 50 seed target readings remain: p001 せいど, p002 しようりょう,
p003 せんい, p004 ごうせい, p005 はれる, p006 ていしょく, p007 いってます,
p008 あき, p009 きばん, p010 しさく, p011 ふか, p012 きのう, p013 かんじ,
p014 しよう, p015 じこ, p016 けいい, p017 いじょう, p018 りようりょう,
p019 じしん, p020 じゅよう, p021 ようい, p022 しょうか, p023 げんてん,
p024 たいせい, p025 しゅどう, p026 こうがく, p027 きゅうでん, p028 ほかん,
p029 しこう, p030 へいこう, p031 じき, p032 きょうちょう, p033 せんとう,
p034 しめる, p035 かみ, p036 あつさ, p037 ひょう, p038 いどう, p039 にちじ,
p040 かち, p041 きち, p042 たいとう, p043 こうしょう, p044 かんしょう,
p045 せいかく, p046 こうせい, p047 たいしょう, p048 かてい, p049 しじ,
p050 かんしん.

## History replay (`--history-replay`)

`accuracy-gate --history-replay <export.tsv>` replays an outside-repository
developer-history export through the same committed-context path. Only counts
are printed; no readings, surfaces or session data leave the run.

- **Measured rows**: `commit` records in `normal` scope whose most recent
  `key` record in the same session has `action = convert` — the convert key
  itself committed the candidate. All other normal commits still join the
  clue chain, because the engine's committed context sees every commit.
- **Clue**: the previous `normal` commit in the same session; its carry is
  derived exactly as the measurement above (`initial_right_id` + bounded
  `cross_commit_tail` bridge).
- **Homophone swap**: a mismatch counts only when the first candidate and the
  committed surface both contain kanji and their non-kanji character sequence
  (the okurigana shape) is identical — "appeared somewhere in the candidate
  list" does not count.
- **Columns** are read by header name (`kind`, `session`, `scope`, `action`,
  `reading`, `surface` are required); the breakdown for readings of four
  characters or fewer is printed alongside the totals.

## Corpus rules

The loader rejects a corpus that would hide or inflate a result:

- a variant equal to the reading (a kana answer for a conversion that did not
  happen), an empty variant;
- a duplicate id, a duplicate reading in `conversion.tsv`, a duplicate
  reading and surface in `prediction.tsv`, a duplicate
  `(clue_reading, clue_surface, reading)` in `context.tsv`;
- an unknown slice, a wrong column count, a wrong header;
- in `context.tsv`: a pair with other than exactly two rows, an empty required
  field, a clue surface the clue conversion does not propose (the gate fails
  at measure time and names the row), or a pair whose two rows disagree on
  `reading` or agree on `expected` — neither shape is a homophone pair;
- in `context.tsv`: an `expected` surface (and none of its variants) that is
  not among the candidates for `reading` — the gate fails at measure time and
  names every such row, because an unreachable answer measures the corpus,
  not the engine. A row that intentionally measures a surface absent from the
  shipped dictionary opts out with `@not-in-dictionary` in `variants`; the
  directive is not a variant, and the reason belongs in this README or the
  row's Issue.

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
- Context: the pair bucket and the lenient/strict counts for both forms, the
  rows whose one-shot or committed-context state changed, and (report only)
  same-state top changes and bridge engagement changes.
- `check: PASS`, or `check: FAIL` with the violated conditions below.

## Failure conditions of `--check`

1. A row's state changed while the snapshot was not updated (conversion: state
   2/1/0; prediction: `full@1`, `full@9`, `half@9` or `k9`; context: the
   one-shot or the committed-context state).
2. A row got worse and `accepted-breaks.tsv` does not list it.
3. A measured count is below its floor.
4. A floor is below the count the snapshot files imply (this stops lowering
   only the floors).
5. An accepted break names an unknown id, or a row that is now at its best
   state (conversion state 2; prediction `full@1`, `half@9` and `k9 = 1`;
   context: one-shot *and* committed-context state 2 — the break stays while
   either form is worse).

A changed top surface with the same state, a reordering inside the nine
suggestions, and a change in whether the clue produced a usable bridge are not
failures.

## Accept a change

1. Run the gate and read the report: every changed row is listed.
2. For each worsened row, add one line to `accepted-breaks.tsv` with the kind
   (`conversion`, `prediction` or `context`), the row id, the Issue (`#123`), a
   one-line reason that states the Issue's measured fixed and broken rows, and
   the date (`YYYY-MM-DD`).
3. Run the same command with `--accept` instead of `--check`. It refuses while
   a worsened row has no entry; otherwise it rewrites the snapshots and sets
   every floor to the newly measured count.
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
