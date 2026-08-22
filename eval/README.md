# Sakura Input Quality Measurement System

機械的に証明できる品質は deterministic test。
日本語としての良し悪しだけを Luna Max。
Luna Max 自体の品質は Human Calibration Set で定量保証する。

このディレクトリは、その評価基盤の資産である。既存の `corpus/`（Mozc 対照の top-1
held-out）とは役割が違う。`corpus/` は expected surface を持つ決定的採点用、
ここは **expected を Judge に渡さない** pairwise 評価と、TSF/状態機械の決定的契約を
一つの release gate に束ねる。

Luna が「日本語として良い」と判定したことを、TSF / candidate UI / timeout /
IPC の証拠にしてはいけない。Issue #68 は engine や既存 TLC が正しくても
`OnTestKeyDown` timeout から Chromium へ Space が返る経路がモデル外だった。
Issue #69 は engine 変換成功と candidate popup 破損が別軸である。

## Architecture

```text
                      ┌─────────────────────┐
                      │  Dogfood / Issues   │
                      │ 実利用で見つけた問題 │
                      └──────────┬──────────┘
                                 │
                     privacy-safe minimize
                                 │
                                 ▼
                    ┌──────────────────────┐
                    │ Evaluation Corpus    │
                    │ versioned / immutable│
                    └──────────┬───────────┘
                               │
              ┌────────────────┴────────────────┐
              │                                 │
              ▼                                 ▼
    ┌──────────────────┐              ┌───────────────────┐
    │ Deterministic    │              │ Semantic Eval     │
    │ Oracle           │              │ Candidate Capture │
    │                  │              │                   │
    │ state / TSF /    │              │ baseline          │
    │ literal / IPC    │              │ candidate         │
    └────────┬─────────┘              └─────────┬─────────┘
             │                                  │
             │                                  ▼
             │                          ┌──────────────────┐
             │                          │ Luna Max Judge   │
             │                          │ Blind A/B        │
             │                          │ structured JSON  │
             │                          └────────┬─────────┘
             │                                   │
             └────────────────┬──────────────────┘
                              ▼
                    ┌───────────────────┐
                    │ Aggregator        │
                    │ CI Release Gate   │
                    │ Trend / Report    │
                    └───────────────────┘
```

Judge そのものは Human Calibration Set で校正する。

```text
Human Calibration Set
        ↓
human label ─────┐
                 ├─ Judge agreement
Luna Max ────────┘  major-error recall
                    false-negative rate
                    drift detection
```

## Luna Max に任せるもの / 任せないもの

任せるのは、日本語話者が IME を使ったときの意味品質だけである。

- 文脈に対して top-1 が自然か
- baseline と candidate のどちらが望ましいか
- 候補順位の改善 / 悪化
- 不自然な漢字変換
- typo repair が助けか、勝手な書き換えか
- カタカナ語、IT 用語、英数字混在の自然さ
- 余計な訂正操作を要求するか

任せないもの（コードで判定する）:

- `consumed=true/false`、Space 二重配送、composition lifecycle
- `TestKeyDown → KeyDown`、timeout、stale revision
- candidate UI ownership、stale popup
- exact literal preservation（hard oracle）
- commit 回数、process leak、engine / renderer provenance
- DLL / engine / dictionary hash、IPC protocol
- invariant、TLA+ / TLC property

## Corpus が資産

テストコードより、versioned / immutable な case が資産になる。

```text
eval/
├─ corpus/
│  ├─ semantic/        Luna が pairwise で見る
│  ├─ behavioral/      Luna に渡さない決定的契約
│  └─ calibration/     Judge 校正。本番評価から分離
├─ judge/v1/           prompt / rubric / schema / identity
├─ fixtures/           Phase 1 の capture / mutant
├─ baselines/
└─ reports/            生成物。リポジトリへは置かない
```

Semantic case は expected output を持ってよいが、**Judge へは渡さない**。
`constraints` と `oracle` は deterministic oracle 専用。Judge view から削除する。

Judge に見せる `case_id` は opaque にする。`issue66-esp32-corruption` のような
ID は正解リークなので禁止。

## Privacy

```text
developer-history
      ↓
anomaly detector
      ↓
bounded extraction
      ↓
delta minimization
      ↓
privacy sanitization
      ↓
human approval
      ↓
corpus
```

raw dogfood log を Luna Max へ送らない。case 内テキストはすべて untrusted data。

## Blind A/B

baseline / candidate、PR / main というラベルは Luna に渡さない。
内部でランダム化し、`SYSTEM_A` / `SYSTEM_B` だけを渡す。
判定は `A` / `B` / `tie` / `ungradable`。絶対スコアは主指標にしない。

決定的な判定は fresh Codex session で A/B を入れ替えて再評価する。
同じ conversation を `resume` しない。両 run が同じ匿名側を選んだら
`POSITION_BIAS / UNSTABLE` であり、release 判定に入れない。

## Runner

Phase 1 の実装は `tools/ime-eval`（crate `sakura-ime-eval`、binary `ime-eval`）。
shipping runtime にはリンクしない。Codex はリポジトリ root から起動せず、
case.json と schema だけを持つ一時ディレクトリから `codex exec` する。

```text
cargo run --locked -p sakura-ime-eval -- identity
cargo run --locked -p sakura-ime-eval -- oracle --capture eval/fixtures/captures/issue66-literal.json
cargo run --locked -p sakura-ime-eval -- capture \
  --baseline-engine target\x86_64-pc-windows-msvc\release\sakura_engine.exe \
  --candidate-engine target\x86_64-pc-windows-msvc\release\sakura_engine.exe \
  --baseline-dictionary path\to\baseline\system.dic \
  --candidate-dictionary path\to\candidate\system.dic \
  --baseline-git <40-hex-sha> --candidate-git <40-hex-sha> \
  --out eval\reports\issue66-capture.json
cargo run --locked -p sakura-ime-eval -- judge --capture eval/fixtures/captures/issue66-literal.json --backend prefer-literal --seed 1
cargo run --locked -p sakura-ime-eval -- judge --capture <capture.json> --backend codex --seed 1 --out eval/reports/run
cargo run --locked -p sakura-ime-eval -- calibrate --labels <calibration.json>
cargo run --locked -p sakura-ime-eval -- gate --results <dir> --profile phase1
```

`capture` is Windows-only because it launches two explicitly owned engine
processes on private test pipes. It sets `SAKURA_DICTIONARY` and
`LOCALAPPDATA` only for those children, verifies the pipe server PID, sends
each case's `input.typing` sequence, and records only candidate text plus
artifact hashes. A missing `typing` sequence, unsupported task, missing
candidate list, timeout, protocol mismatch, or cleanup failure aborts the run;
it never falls back to a synthetic or ambient engine capture.

必須 identity: git SHA、engine / dictionary SHA-256、Judge の model / reasoning /
Codex CLI version / prompt / rubric / schema SHA-256、corpus manifest SHA-256。

required:

```text
model = gpt-5.6-luna
reasoning = max
```

`max` が使えない場合に `xhigh` へ downgrade しない。Judge environment invalid として FAIL。

## Release gate（初期値）

| Gate | Rule |
|------|------|
| GATE-01 | Deterministic failure == 0 |
| GATE-02 | Literal corruption == 0 |
| GATE-03 | Severity 4 semantic regression == 0 |
| GATE-04 | Severity 3 semantic regression == 0 |
| GATE-05 | Material semantic regression rate の Wilson 95% 上側 ≤ 1.0% |
| GATE-06 | Judge unstable rate ≤ 3% |
| GATE-07 | Human calibration agreement ≥ 90% |
| GATE-08 | Major regression recall ≥ 95% |
| GATE-09 | Judge identity fully known |
| GATE-10 | Artifact + dictionary identity fully known |

Phase 1 profile は GATE-01, 02, 03, 04, 06, 09, 10 と集計機械を検証する。
GATE-05 は Wilson 上側がサンプル数に依存するため release profile のみ。
GATE-07 / 08 は calibration 投入後の release profile。

Release safety と Quality improvement は分ける。改善主張には decisive pairwise
で candidate preference の 95% CI が 50% を超えることを要求する。

## Implementation phases

1. Judge 基盤（本ディレクトリと `ime-eval`）
2. Human Calibration Set（300、層化、200 / 100 holdout 分離）
3. Issue #66 semantic corpus を vertical slice として拡充
4. Dogfood → sanitized fixture
5. Issue #67 TSF Contract E2E
6. Nightly / Release（installed artifact provenance を含む）

Judge の model、reasoning、Codex CLI version、developer instructions、rubric、
schema、aggregation algorithm のどれかが変わったら Judge の新 version とし、
Calibration Set を再実行する。

## Conversion Quality Program — Stage 1

The user-provided 50 conversion examples live in
`corpus/behavioral/conversion-quality-stage1/fixture.json`. They are
deterministic challenge observations, not semantic Judge cases and not
unconditional Top-1 assertions. The fixture stores slash-free surfaces and
separate segment arrays so surface equality and boundary equality remain
independent.

`ime-eval quality-core-capture --fixture FIXTURE
--baseline-dictionary DIC --candidate-dictionary DIC --baseline-git SHA
--candidate-git SHA --evaluator EVALUATOR --out CAPTURE` invokes the existing
`sakura-core::Converter` once for each full reading and records at most 18
whole-reading candidates, available segment sequences, core evaluator/dictionary
identity, and bounded runtime metadata. Learning, user dictionary, reranker,
and input repair are disabled by the fixed Stage 1 options.

Then `ime-eval quality-score --fixture FIXTURE --capture CAPTURE --out REPORT`
produces the versioned report described by
`quality/v1/quality-observation.schema.json`. It records Top-1, Recall@18,
MRR@18, explicit segment exactness, negative controls, artifact/options
identity, and capture terminal/truncated/elapsed metadata. `quality-score`
accepts only the `whole_reading_core` lane.

`ime-eval quality-capture --fixture FIXTURE --baseline-engine ENGINE
--candidate-engine ENGINE --baseline-dictionary DIC --candidate-dictionary DIC
--baseline-git SHA --candidate-git SHA --out CAPTURE` remains a diagnostic
real-engine replay. It captures active-segment UI candidates and is not a
whole-reading quality input; passing it to `quality-score` is rejected.

The Stage 1 options profile is exactly `quality-stage1-default`, keeps the
production candidate bound at 18, and turns off learning, user-dictionary,
and reranker inputs for the deterministic baseline. Each case retains its
`assertion_scope` (`candidate_observation`, `context_required`, or `hold`) in
the generated report. The 50 cases stay outside the Judge loader, and
expected targets must never enter a blinded prompt. The checked-in
`baselines/quality-stage1-v1.json` is a generated
`quality-observation.schema.json` scoreboard for an observational
`whole_reading_core` baseline; it is explicitly not a release gate or a
context-free Top-1 gold set.
