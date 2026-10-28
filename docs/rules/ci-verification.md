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
  SIMD kernel tests only exercise kernels the host supports, and `cargo test`
  captures stdout, the first two runs covered AVX-512 or did not with nothing
  readable afterwards to tell the two apart — worse than a known gap, and the
  same failure as a workflow that never runs.

  Fixed by making each run state its own scope: a CI step re-runs `width::scan::`
  with `--nocapture` so the log prints `kernels under test: [...] (tier ...)`.
  It paid for itself immediately by producing the fourth row above and
  refuting the inference in the second. **Quote that line, never the CPU
  name, when claiming a kernel was covered.**

  **AVX-512 verification is local, by the owner's decision (2026-07-31)**, and
  CI is not to be extended to *require* it — the step above only reports what
  happened to be covered. The standing obligation is therefore to run
  `cargo test -p sakura-core --lib -- width::scan:: --nocapture` on this machine
  (before #206 Phase 3.3 the filter was `simd::`; a filter naming a moved
  module matches zero tests and still exits 0, so check the count)
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

- **ワークスペースのツールがエンジンの test-only feature に依存するときは、
  ツール側も opt-in の feature にする。** Cargo は `--workspace` のビルドで
  feature を統合する。ツールが `sakura-engine/replay-eval` を常に有効にすると、
  出荷する DLL とエンジンにもその API が入る。#305 の `learning-replay` は
  `required-features = ["learning-replay"]` と optional 依存にした。
  `cargo tree -e features --workspace` で、`--features` なしのビルドに
  `replay-eval` が出ないことを確認した（2026-10-16）。CI と release の
  clippy・test の feature リストにも、その feature を足す。

- **`gh pr checks --watch` と `gh run watch` の終了コードは、CI の結果ではない。**
  2026-10-16（#357）に、どちらも一時的な `wsarecv` のタイムアウトで exit 1
  になったが、run は続いていた。`gh run view <id> --json status,conclusion`
  を、失敗した呼び出しを許すループで確認する。

- **`scripts/publish-release.ps1` は、署名の前の `gh attestation verify` が
  ネットワークエラーで失敗すると、何も作らずに止まる。** 2026-10-16（#357）に
  TLS handshake timeout で止まったとき、`gh release view v2.0.17` は
  "release not found" で、候補フォルダーは installer と manifest の2ファイル
  のままだった。この2点を確認してから同じコマンドを再実行し、27 秒で公開と
  読み戻しまで終わった。

- **起動時の回復の判定表は、「種類 × 個数（0、1、2 以上）」のマスごとに
  テストを置く。** 2026-10-18（#360 A4）に、学習ログの回復で `compact.bak`
  が 2 つあるときのテストはあったが、`v1.bak`/`v2.bak` が 2 つあるときの
  `InvalidData` の分岐にはなかった。独立した検証者が「バージョン付きの
  バックアップが複数あるときは先頭を戻す」改変を入れても、sakura-store と
  sakura-engine の 307 件はすべて通った。d024dd3 で 2 件のテストを足すと、
  同じ改変が実行時に検出された。正式なファイルがない場合の「別の名前の
  ファイルに触れない」も、ある場合とは別に確かめる。

- **故障注入の項目は、テストで元のコードと区別できると確かめてから rubric に載せる。**
  2026-10-18（#360 orig#1）に、「`record.sha256` とだけ大文字小文字を無視して
  比べる」改変は先に走る pin 検査のため等価変異で、「ハッシュ後に読み直して
  返す」改変は単体テストで観測できなかった。こうした項目はコードを読む確認にする。

- **テストを足したら、fail-before の手順が修正前のコードでまだコンパイルできるか
  確かめ直す。** 2026-10-18（#360 orig#1）に、新しい定数を参照するテストが
  修正前のビルドを壊し、独立検証の R5 が不合格になった。修正前の本体に定数
  2 行だけを足す手順に直して合格した。

- **一時ディレクトリの外に作るテスト用の固定物にも、それぞれ drop guard を付ける。**
  2026-10-18（#360 #2）に、base の外に作った junction の参照先は、assert が
  失敗するたびに `%TEMP%` に残った。参照先にも `TestBase` を付けて base より
  先に宣言すると残らなくなった（35fda2f）。

- **利用者向けドキュメントの事実は、修正の要約ではなくコード経路（呼び出し元、
  すべての分岐、選ぶ順序）で確かめる。** 2026-10-18（#360 A4/A5）に、依頼文から
  書いた「学習ログの更新は 1 回の置換」（追記と、rename 2 回の forget がある）と
  「TOTP 確認が使えないまま残ることはない」（`Corrupt` と I/O エラーの経路がある）
  が誤りだった。「必ず」「すべて」「ない」の言い切りから先に確かめる。

- **消す対象を列挙する関数は、一覧の失敗を空リストにしない。** 2026-10-18（#360 A3）に、
  `read_dir` の失敗を空として返したため `clear` がコピーを残したまま公開した（T9）。
  `io::Result` で返し、best effort の呼び出し元だけが明示的に捨てる。古い順に消すとき
  は同じ呼び出しで作ったものを先に除く（時計が戻ると名前順が逆転する、T8）。ファイル名
  の `str` を固定バイト位置で切らない（`split_at(16)` が日本語名で panic した）。

- **別のスレッドやプロセスが書くファイルを待つテストは、ファイルが現れるのではなく、
  中身がそろうのを待つ。** 2026-10-28（#419）に、`pad_storage` の一時停止フックの
  テストが、ワークスペース全体のテスト実行で 1 回だけ落ちた（再実行は緑）。通知
  スレッドが 10 ms ごとに `exists()` を見て、見えたらすぐ中身が `ready` だと assert
  したが、フックは `create_new` で作ってから `write_all` で書くので、その間は
  ファイルが空で存在する。空を読むと assert が落ち、`.resume` を書く前にスレッドが
  終わるため、フックが予算の 2 秒を待って `TimedOut` を返した（二つ目の panic は
  結果）。窓は、読み取りだけを繰り返すと 1500 回中 80〜111 回で空に当たる広さがあり、
  10 ms 周期の通知スレッドでは 12000 回に 1 回だった。通知スレッドが `ready` に
  なるまで読み直す形（`wait_for_ready_signal`）に直すと 12000 回で 0 回になった。
  直す側は、途中の状態を作るのが本番にある書き方（このフックの二段階の公開）なら
  読む側、テストだけが作る書き方（#379 の監視対象への `fs::write`）なら書く側である。
  自然な失敗率が低いので、直ったことは率ではなく、空のファイルを 150 ms 置いてから
  書く決定的な回帰テストと、待ちを存在だけの判定にする変異・期限で `true` を返す
  変異の検出で示した。

- **ロックの検証は、並行書き込みの負荷テストではなく、ロックをテスト自身が握る
  テストで行う。** 2026-10-27（#406）に、`debug.tsv` の世代交代を 8 スレッドと
  4 プロセスの同時書き込みで試したが、ロックを外しても両方とも緑のままだった。
  ロックの中で行うサイズの再確認が、競合の大半を吸収するためである。落ちたのは、
  ロックファイルをテストが握って `WouldBlock` を期待するテストだけだった。再確認を
  外した変異は、プロセステストが「世代は満杯のときだけ捨てられるので、前の世代が
  小さいことはない」という不変条件を検査するようにして、初めて検出できた。壊れる
  不変条件に名前を付けて検査し、変異を走らせてから、負荷テストの緑を信じる。

- **間欠的に落ちるテストは、assert に触る前に、応答の裏にある本当のエラーを記録して、
  テストの競合か製品の欠陥かを決める。** 2026-10-28（#417）に、
  `live_engine_hot_enables_developer_history_and_records_a_normal_key` は、孤立した
  実行の約 3 割（9/30、11/30）で `ClearInputHistory` が `Internal` を返して落ちた。
  失敗した 11 回で本当のエラーを記録すると、毎回 `AlreadyExists`（os error 80）で、
  退役中の writer がストアのロックをまだ握っていた。テストに待ちを足す直し方は、
  退役が 6〜28 ms、ときどき 270 ms を超えるため確率を下げるだけで、開発者モードを
  切った直後に `diagnostics clear` が失敗する製品の欠陥が残る。
  `HistoryRuntime` で退役の完了を待つ直し方にした。

- **待ってから作用する（check-then-act）バリアは、待ちが終わった後に状態が変わる
  隙間を、作用が済むまで同じ保持で覆い、隙間は実際にそこで止めるテストで見る。**
  2026-10-28（#417）の最初の直し（待つだけの版）は、待ちを終えてから Clear を
  ディスパッチするまでの間に、開発者モードの切り替えが入る隙間を残した。切り替えが
  接続済みのサービスを退役させると、停止済みの書き込みスレッドへ送って `BrokenPipe`、
  新しい世代を始めると、そのロックとパス指定の Clear が衝突して `AlreadyExists`、
  どちらも `Internal` になる。PR #418 のレビューで指摘され、`before-dispatch`
  の fault injection で Clear を 3 秒止め、その間に設定を公開する end-to-end テスト
  2 本で、直す前は 2 本とも `Internal` で落ちることを確かめてから、待機を
  `HistoryRuntime::hold_idle` が返す保持（`LifecycleHold`）に置き換えた。保持の間は
  ワーカーが退役も初期化もせず、Clear は保持を取った時点のサービスに対して
  ディスパッチされる。止められない隙間は、保持を取ってからサービスを読むまでの
  数マイクロ秒（`before-dispatch` はその後ろにある）で、ここは保持と同じ臨界区間で
  サービスを複製する作りと、`runtime_services` が保持のサービスを返す単体テストで
  覆う。`serve()` が保持を渡し忘れる変異は、決定的には検出できない。

- **実エンジンへ設定変更を公開して競合を再現するテストは、先に監視へ既存のファイルを
  一度読ませる。** 2026-10-28（#417）に、設定ファイルがまだ無い間は engine の監視が
  待ち時間を 500 ms から最大 30 秒まで倍にするので、起動直後に公開した OFF→ON の
  変更が Clear の後ろに着き、直す前の製品に対しても `Ok` で通った。開発者モードを
  一度 ON にして効果（ロックファイルの出現）を待ち、OFF にして解放を待つ周回を
  前置きすると、直す前は `Internal` で落ちるようになった。落ちることを確かめる前の
  緑のテストは、再現できていることの証拠にならない。

- **ファイル監視のテストは、監視対象を `fs::write` で書き換えず、一時ファイルへ
  書いてから rename で置き換える。** 2026-10-28（#379）に、`fs::write` は先に
  ファイルを切り詰めるため、5 ms 間隔の読み取りが切り詰めと書き込みの間に当たると
  空ファイルを読む。空の設定は有効な既定値として解釈されるので、本物より先に公開
  されて、テストは別のスナップショットを受け取った（`normalizer.width.alnum` が
  `Full`）。本番の書き手（settings の `atomic_write`、engine の `upgrade()`、
  `publish_test_configuration`）はすべて原子的に置き換えるので、テストだけが現実に
  ない中間状態を作っていた。`replace_configuration` を二つの監視テストで使い、
  アサーションは変えていない。

- **ゲートや channel でワーカーを止めるテストは、assert が失敗しても必ず開く guard を
  付け、その guard を止められる側の所有者より後に宣言する。** 2026-10-28（#417）に、
  opener を channel の `recv()` で止めるサーバーのテストが、Sender を
  `HistoryRuntime` を持つ `Shared` より先に宣言していた。Rust は宣言と逆順に drop
  するので、最初の assert が落ちると `Shared` の drop がワーカーの join で戻らず
  （Sender はまだ生きている）、変異の検証でテスト実行ファイルが 3 分以上固まった。
  Sender を後に宣言して直し、`history_runtime` のテストには drop で開く
  `OpenOnDrop` を付けた。

- **サービスが付いていないと実ユーザーの領域に作用する Request は、プロセス内の
  テストでディスパッチしない。** 2026-10-28（#417）に、履歴サービスなしの
  `ClearInputHistory` は `%LOCALAPPDATA%` の本物の `history\input.bin` をパスで
  消すため、待つかどうかの判断だけを `Shared::hold_history_lifecycle` で検査し、
  実際の Clear は、私有の `LOCALAPPDATA` を持つ engine の子プロセス
  （`pipe_round_trip`）だけが送る。
