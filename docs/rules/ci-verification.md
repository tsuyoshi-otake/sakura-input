## CI and verification

- **A workflow that never runs is indistinguishable from a workflow that
  passes.** `ci.yml` and `installer.yml` both triggered on `push: branches:
  [master]` while this repository's only branch is `main`. `gh run list`
  returned nothing: neither had run on any commit, and several "CI is green"
  assumptions were assumptions about a workflow that was not executing. Check
  `gh run list` after adding or editing a workflow, not just the YAML.

- **A processor's model name is not evidence about the ISA available to your
  process. Only a feature probe is.** This was got wrong twice in one hour,
  each time by reasoning from the name printed by `Get-CimInstance
  Win32_Processor`:

  | run | reported processor | inferred | **measured** |
  |---|---|---|---|
  | `6848ac5` | AMD EPYC 7763 (Zen 3) | no AVX-512 | not printed |
  | `e829ff9` | AMD EPYC 9V74 (Zen 4) | AVX-512 | not printed |
  | `577a550` | Intel Xeon Platinum 8573C | AVX-512 | **`tier avx512bw`** |
  | `6905660` | AMD EPYC 9V74 (Zen 4) | AVX-512 | **`tier avx2`** |
  | `173c216` | AMD EPYC 7763 (Zen 3) | no AVX-512 | **`tier avx2`** |

  The fourth row is the point. Zen 4 has AVX-512 in silicon, and that runner
  still reports `avx2` — the hypervisor does not expose it to the guest.
  `is_x86_feature_detected!` knows that; a datasheet does not. The fifth row
  is the same lesson from the other side: an inference that happened to be
  right is still not a measurement until the log prints one.

  Of five runs, three printed a kernel list and **one** covered AVX-512. Treat
  CI coverage of that kernel as occasional, never as given.

- **`windows-latest` is not one machine, so a green CI run's differential
  SIMD coverage is not a fixed quantity.** Four runs of one workflow inside
  an hour drew three processors and two different ISA tiers. Since the
  `simd::` tests only exercise kernels the host supports, and `cargo test`
  captures stdout, the first two runs covered AVX-512 or did not with nothing
  readable afterwards to tell the two apart — worse than a known gap, and the
  same failure as a workflow that never runs.

  Fixed by making each run state its own scope: a CI step re-runs `simd::`
  with `--nocapture` so the log prints `kernels under test: [...] (tier ...)`.
  It paid for itself immediately by producing the fourth row above and
  refuting the inference in the second. **Quote that line, never the CPU
  name, when claiming a kernel was covered.**

  **AVX-512 verification is local, by the owner's decision (2026-07-31)**, and
  CI is not to be extended to *require* it — the step above only reports what
  happened to be covered. The standing obligation is therefore to run
  `cargo test -p sakura-core --lib -- simd:: --nocapture` on this machine
  before releasing anything that touches the kernels. Since production now
  keeps AVX-512 bench-only, confirm the printed `kernels under test` includes
  the scalar, AVX/SSSE3, AVX2, and all three AVX-512BW+VL threshold variants;
  `resolved width scan avx2-hybrid` is the intended shipping selection, not a
  coverage failure. Verified here on 2026-08-22.

- **A `cargo test` filter that ends in `::` makes an unquoted YAML `run:`
  line unparseable, and GitHub reports it as anything but a syntax error.**
  `run: cargo test -p sakura-core --lib -- simd:: --nocapture` puts a colon
  immediately before a space, which is YAML's mapping separator: the file
  stops parsing at that line (`mapping values are not allowed here`, line 56
  column 54). What GitHub then showed was **the whole workflow having no
  triggers** — `gh run list` named the run `.github/workflows/ci.yml` instead
  of `CI`, it ran **zero jobs**, `gh run view --log-failed` said "log not
  found", and `gh workflow run ci.yml` refused with HTTP 422 *"Workflow does
  not have 'workflow_dispatch' trigger"* about a file that plainly has one.
  Recognise that signature: it means unparseable, not misconfigured.

  Quote any `run:` value containing `: `. And parse workflow files locally
  before pushing — `~/tmp/yamlvenv/Scripts/python.exe` has `pyyaml` for
  exactly this; a red run is a cheap way to find out, but a run that never
  starts teaches nothing on its own.

- **A sandbox test that does not prove it is sandboxed proves nothing.** A
  test that connects to the pipe "from an AppContainer" passes just as
  happily when the AppContainer was never applied. The child must assert
  `TokenIsAppContainer` on its own token before it does anything else.

- **A test that leaks a watchdog corrupts the *next* run, not its own.**
  `tests/watchdog_recovery.rs` kills the engine and waits for the renderer to
  restart it, with a no-renderer control phase to prove nothing ambient does
  the restarting. An early version leaked its renderer, and the following
  run saw an engine reappear 12.75 s after the kill — about one
  `WATCH_BUDGET` — with no renderer of its own started. The control's 5 s
  window missed it, so the test would have passed for entirely the wrong
  reason. Two fixes, both structural: refuse to start when **any** Sakura
  process is running (a renderer holds no pipe, so only the process list
  finds it), and tear down the watchdog *before* the thing it watches, or it
  dutifully restarts what the teardown just stopped.

- **Verify a test can fail before believing it passed.** Commenting out the
  renderer spawn made `watchdog_recovery` fail after 30 s with the intended
  message. Without that run, "it passed" would have been indistinguishable
  from "it cannot fail".

- **A test that configures a setting to a non-default value has not tested
  the default.** Issue #99's two dispatch tests for the punctuation family
  each set the role they exercised to a non-default mark (`FullWidth` comma,
  `HalfWidth` period). Both passed. Under the shipped `Touten`/`Kuten` style
  the configured mark *is* the reading, and that is the one case where the
  ranker moved the selection off the configured row — so every test was green
  and every real installation was wrong. Pick fixture values because they hit
  the interesting case, not because they are visibly different from the
  default, and cover `PunctuationStyle::ALL`-style enums by iterating.

- **Candidate *order* and candidate *selection* are separate contracts owned
  by separate code.** The converter builds the list; `preferred_candidate_index`
  and `preserve_exact_initial` in `dispatch.rs` decide what is highlighted.
  Fixing one leaves the other free to produce "the list is right but the wrong
  character lands in the document", which is the hardest failure for a user to
  describe. Assert both.

- **Suppressing learning on the way *in* is not the same as ignoring it on the
  way *out*.** `synthetic_exact` kept the punctuation rows out of the learning
  store, but surfaces learned under an *earlier* setting still outranked the
  configured mark on every later conversion. A feature that declares one
  durable preference has to close both directions.

- **本番コードが per-user の永続ファイルへ書くなら、その writer にテスト用の
  差し替え経路を用意する。** `sakura-tsf` の `note_timeout` / `note_disconnect`
  は `#[cfg(test)]` でシステム temp へ向き先を変える。これを忘れた writer が
  1つでもあると、`cargo test --workspace`（現在 1,721 テスト）が実ユーザーの
  `%LOCALAPPDATA%` 診断プロファイルへ追記する。テストが汚れるのではなく**本番
  の観測データが汚れる**問題で、被害はテスト終了後に残り、しかもその数値を
  根拠に次の判断をしてしまう。新しい diagnostics writer を足すときは、既存
  writer の `#[cfg(test)]` 版を必ず一緒に写す。

- **同じ終了処理を複数の理由で呼ぶ関数は、理由を必須引数にする。** #102 では
  `TextService::disconnect()` が引数なしで、engine セッションを捨てる 21 の
  production 経路が区別できなかった。optional な注釈にすると必ず「あとで
  埋める」経路が残る。必須引数にすれば、新しい経路の追加が名前を決めるまで
  コンパイルエラーになる。

- **同じ形式の bounded log を2本持つときは、種別を分けているものが何かを明示
  してテストで固定する。** IPC diagnostics では timeout と disconnect の wire
  code 1..10 が**両方で有効**であり、分けているのは 4 byte の magic だけ
  である。誤って相手のリーダに読ませると、別の意味のカウンタが黙って増える。
  「片方のログをもう片方として読んでも 1 件も計上されない」を両方向で固定
  する。

- **理由を捨てている行を直すだけでは足りないことがある。情報を生成している行
  まで遡る。** #104 では `client.rs` の `verify_server_process(...).is_err()`
  が容疑だったが、`verify_server_process` の 5 step はすべて
  `ERROR_ACCESS_DENIED` になり得て、うち 2 つはその HRESULT を**自分で合成**
  していた。呼び出し側で理由を通しても step は区別できないままだった。
  「どこで情報が失われたか」は、捨てている行と生成している行の両方を見る。

- **variant 名が主張になっている error 型を、手近だからという理由で流用しない。**
  `sakura-renderer` の `PipeBinding::connect` は、自分の `current_exe()` が
  読めない／install root へ親辿りできないという**接続前**の失敗に対して
  `Fault::UntrustedServer { process_id: 0 }` を返していた。peer を判定して
  いないのに untrusted を名乗るので、ログがそのまま虚偽になる。判定していない
  ことを言う variant（ここでは `ServerRejection::PolicyUnavailable`）を用意する。
