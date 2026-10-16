## Issue #305 消して打ち直した訂正を学習に反映する（設計）

- 状態: 実装対象（2026-10-16、owner が設計後の実装・リリース・再インストールを指示）
- 対象: `crates/sakura-engine`（訂正の検出、学習、順位付け）、`crates/sakura-store`（学習ログの記録）、`tools/conversion-eval`（再生評価）
- 個人の入力内容は載せない。例は辞書語だけを使う。

### 1. 目的と範囲

#### 直すもの

確定を消して同じ読みを打ち直し、別の表記にした操作（以下「訂正」）を検出する。そして、消した語があった文脈で、誤った表記 X が学習によって先頭に戻らないようにする。打ち直した表記 Y は、その文脈で先頭になるようにする。

#### 直さないもの（明示）

1. **気づかずに残した誤変換。** ユーザーが一度も直さなかった確定は、エンジンからは正しい確定と区別できない。そのため今後も学習される。Issue の題名にある症状のうち、この設計が扱うのは「気づいて直した」場合だけである。残る部分の緩和策は次のとおり。
   - 30 日の半減期（`LEARNING_HALF_LIFE_DAYS`）。
   - その文脈で一度でも訂正すれば、この設計で直る。
   - 候補を個別に削除する操作（別 Issue）。
2. **ほかの文脈での X。** 打ち消しは消した語の左文脈 L だけに効く。ほかの文脈では、X の general（文脈なし）の学習が今までどおり効く。general を消す方式は、予備計測で悪化が増えたため採らない（改善 22、悪化 6、辞書順に戻った変換のうち元が正解だったもの 4）。
3. **打ち直した直後の再変換の表示順。** 消した X を打ち直しで下げることはしない。消して同じ表記を打ち直す操作（147 回）が訂正（128 回）より多いためである。消した X は確定キャッシュに残るため、同じセッションの打ち直しでは今までどおり X が先に出ることがある。
4. **打ち直さずに消しただけの確定。** #275 の猶予で、確定直後に消した確定は学習されない。その猶予をすり抜ける次の 2 経路は、打ち直せばこの設計の訂正として直る。2.0.10 以後、全部消された確定 243 件のうち学習に残ったのは 3 件で、頻度が低い。猶予の延長（Issue 本文の項目 3）は保留のままとする。
   - 確定を 2 回続けてから両方を消す。
   - 消す前に別の変換を始める（`confirm_all_staged`）。
5. **予測候補（入力途中の補完）の並び。** tombstone は変換の順位付け（`preference`）だけに効き、予測履歴（`PredictionHistory`）は変えない。予測候補は一覧で選んだものだけが確定する（`commit_selected_suggestion`、`commit_suggestion_at`）。そのため、先頭の予測が誤っていても、それだけで誤確定にはならない。予測から X を消したいときは、今ある予測候補の削除（Ctrl+Delete、`forget_prediction_exact`）を使う。この削除は X の tombstone も消す（4.4）。

### 2. 現状と原因

#### 2.1 関係するコード（2.0.15、`origin/main` ea299dd）

- **順位付け:** `crates/sakura-engine/src/learning.rs` の `best_candidate`（371-473 行）が、exact（左文脈・読み・表記・右ID）と general（左文脈 0）の 2 回の検索で使われる。
  - 競合の検出（414-423 行）は、位置ゲート（431 行、`maximum_candidate_index` 135-144 行）より前に行われる。
  - 異なる表記に exact の学習が 2 つ以上あると、最後に学習した表記が位置ゲートを無視して勝つ（段階 1、933399e）。
  - exact の学習が 1 件（Weak）なら、候補 2 番目（index 1）までしか上げられない。
- **学習の表:** `Index` は 3-way の集合連想表である。満杯の bucket では `sequence` が最小の slot を上書きする（286-295 行）。
- **猶予（#275、#277）:** `StagedCommit`（602-612 行）は学習を 1 確定ぶん保留する。保留中の記録の扱いは次のとおり。
  - `stage`（850-896 行）: 同じ owner の前の記録を学習する。満杯なら、取り消し待ちでない最古の記録を学習する。
  - `confirm_all_staged`（958-976 行）: 取り消し待ち以外をすべて学習する。
- **確定時の呼び出し:** `crates/sakura-engine/src/dispatch.rs` の `record_learning`（2578-2624 行）。学習対象なら `stage`、そうでなければ `confirm_staged` を呼ぶ。カナ変換（無変換、F6-F10）は学習対象ではない（4920-4939 行）。
- **idle キーの処理:**
  - `settle_staged_learning_after_idle_key`（2636-2659 行）: 取り消し待ちなら `hold_staged`、合成中なら何もしない。それ以外では、未処理の idle Backspace（修飾キーを問わない）なら `discard_staged`、ほかのキーなら `confirm_staged` を呼ぶ。
  - #273（2821-2829 行）: 文字入力以外の idle キーで左文脈を 0 に戻す。
- **確定の取り消し（Ctrl+Backspace、確定直後だけ有効）:** 次の 2 つの処理がある。
  - `crates/sakura-engine/src/session.rs` の `undo_commit`（1357-1387 行）が、左文脈を確定前の値に戻して読みを復元する。
  - ホストが削除に成功すると、`acknowledge_undo_commit`（1395-1410 行）が確定キャッシュから外す。

#### 2.2 ケース別の原因

| 読み | 残っている誤学習 | 段階 1 で直らない理由 |
| --- | --- | --- |
| めんしょん | 面ション（左文脈 0、exact 8 件、general 8） | 訂正がすべてカナ変換で学習されないため、競合が起きない。exact を消しても general（Strong、index 5 まで）で先頭に戻る |
| ずみ | ズミ（左文脈 1841） | #273 で消去後の左文脈が 0 になる。そのため訂正の 済 は左文脈 0 に学習され、1841 の ズミ と競合しない |
| ずみ（10-15） | ズミ を確定し、続けて で を確定してから両方消した | で の `stage()` が ズミ を学習した（#275 の抜け道） |

学習を使わずに同梱辞書と reranker だけで変換すると、メンション・済み・返信 が先頭になる（`neural-eval --mode all-normal --exploratory`、3/3）。先頭が誤る原因は学習だけである。

### 3. 用語

| 用語 | 意味 |
| --- | --- |
| r | 読み |
| X | 消された確定の表記（誤り） |
| Y | 打ち直して確定した表記 |
| L | X を確定したときの左文脈（`carry_right_id`）。訂正が連鎖しても最初の L を引き継ぐ |
| R_X | X を確定したときの右ID |
| trail | セッションごとの直近の確定の記録（4.1） |
| tombstone | 「(r, L) では X を学習で先頭に上げない」という記録（4.3） |

### 4. 設計

#### 4.1 訂正の検出: `CorrectionTrail`

- **所有者と配置:**
  - `Session` がフィールドとして所有する。ロジックとテストは新しいモジュール `crates/sakura-engine/src/correction.rs` に置く。
  - dispatch はイベントを渡し、結果（`Option<Correction>`）を受け取るだけにする。
- **対象:** 学習と同じ条件で、`record_learning` が学習ストアに記録しうる確定だけを対象にする。次の場合は trail に入れず、trail を空にする。
  - 実行方針またはホスト方針が永続化を許さない（`record_learning` が何もせずに戻る場合）。
  - scope が機微（`scope_is_sensitive`: Password、URL、Email、Digits）。
  - scope が未分類でも、学習は今も行われる。そのため trail も対象にする。
- **文書のモデル:** カーソルの左に「残っている確定」が古い順に並ぶ。丸ごと消えた確定は「消えた確定のスタック」に積む。スタックの一番上は、カーソルのすぐ右で消えた確定で、次に打つ文字が置き換えるのはこの確定である。何を消したかが少しでも不確かなときは、照合しない側に倒す。
- **データ（固定長、割り当てなし）:**
  - `entries`: 合わせて最大 4 件。前半が残っている確定（古い順）、後半が消えた確定のスタック（下から上）。
  - 各項目には次の値を持たせる。
    - 表記の文字数（`chars`、Unicode スカラー値の数）。
    - 読み・表記（`FixedStr<96>`）。
    - L（`origin_left`）、右ID。
    - 連鎖で引き継ぐ却下表記（表記 `FixedStr<96>` と右ID を最大 2 件）。
  - 読みか表記が 96 バイトを超える確定は、文字数だけ持つ「追跡しない項目」として入れる。Backspace の数え合わせを崩さないためである。この項目は照合しない。
  - `erase_overflow`: 残っている確定を丸ごと覆わない、余った Backspace の数。
  - 大きさは 1,840 バイト（`size_of::<CorrectionTrail>()` の実測）。`Session` は今 11,976 バイトで、上限テスト（16 KiB、`phase1_session_and_dispatcher_layout_stay_small_for_worker_stack`）に収まる。
- **イベントと状態遷移:**

| イベント | 条件 | 遷移 |
| --- | --- | --- |
| E1 確定 | 永続化対象の確定（学習対象か否かを問わない） | `erase_overflow > 0` なら trail を空にする → 照合（下記）→ Y を残っている確定の末尾に追加（満杯なら、最も古い残っている確定、なければスタックの一番下を押し出す）→ `erase_overflow = 0` |
| E2 消去 | idle → idle の未処理の Backspace。修飾キーなし、または Shift だけ（どちらもホストでは 1 文字の削除） | `erase_overflow += 1`。最新の残っている確定の `chars` を覆う間、その確定をスタックの一番上に移し、`erase_overflow` から引く |
| E3 取り消し成功 | `acknowledge_undo_commit` が true | 最新の残っている確定の表記が取り消した表記と一致すれば、スタックの一番上に移す。一致しなければ trail を空にする |
| E4 連続の終了 | idle → idle のキーのうち E2 以外のすべて（矢印、Enter、Delete、Tab、idle Space、Ctrl / Alt 付きの Backspace（単語削除など、消える文字数が分からない）、Ctrl+Z、モード切替のキーなど） | trail を空にする |
| E5 文脈の破棄 | `clear_personal_context`（`ResetDocumentContext`、scope 変更、機微な確定など）、セッションの reset と終了 | trail を空にする |
| E6 取り消し失敗 | 取り消しの結果が Rejected / Unknown | trail を空にする |

- **取り消しの窓の Ctrl+Backspace:** 確定直後の Ctrl+Backspace は、エンジンが取り消しとして処理する（処理済みのキー）。`settle_staged_learning_after_idle_key` は取り消し待ちを最初に調べて `hold_staged` を呼び、そのキーでは trail を変えない。trail は結果の通知で E3 か E6 になる。窓の外の Ctrl+Backspace は処理されずにホストへ渡るので、E4 になる。
- 新しい合成を始めるキー（idle → 合成中）と、合成中のキーは trail を変えない。プローブ（`ProbeKey`、実行方針 Probe）も変えない。
- モード切替の要求（`SetMode`）は文書もカーソルも動かさないので、trail を変えない。キーによるモード切替は、実装を単純にするため E4 として扱う。消してから打ち直すまでの間にモードを切り替える操作はまれである。
- **照合（E1）:** スタックの一番上が追跡している項目で、その読みが r と等しいときだけ照合する。
  - 却下表記を作る: その項目が引き継いだ却下表記に、その項目の表記（Y と違うときだけ）を加える。重複と Y を除き、最初と最新の 2 件まで残す。Y を除くのは、X→Y→X のように元に戻した場合に、選んだ表記を却下しないためである。
  - 却下表記が 1 件以上: 訂正 `Correction { origin_left: L, rejected }` を返す。同じ表記の打ち直しでも、引き継いだ却下表記があれば訂正になる。訂正の確定 Y を猶予中に消すと、その記録は捨てられるので（4.2）、打ち直しが却下を引き継ぐ必要がある。
  - 却下表記が 0 件（同じ表記の単純な打ち直し）: 訂正ではない。
  - どちらの場合も、Y の項目はその項目の L と却下表記を引き継ぐ。スタックの残りは、続けて打ち直す分としてそのまま残す（2 語を消して左から打ち直す場合）。
  - 一致なし: スタックを全部捨てる（新しい文字が、消えた確定を置き換えたため）。Y の項目の L は現在の左文脈にする。
- **項目の終端:** 照合された、押し出された、一致なしで捨てられた、E1 / E4 / E5 / E6 で空にされた、セッションが終わった、のどれか。どの終端でも学習には触れない。学習への影響は、照合で返した `Correction` を通じた 4.2 の経路だけである。
- **数え方の限界:**
  - ホストが 1 回の Backspace で消す単位（書記素クラスター、UTF-16 単位など）が Unicode スカラー値と違うと、覆う数が合わない。その場合は照合しない側に失敗する。
  - 選択範囲の削除もエンジンからは 1 文字に見える。そのため数が合わなければ照合せず、合えば誤照合しうる（7 章）。
  - 2 語を消して 1 回の変換で打ち直す（ずみ と で を消して ずみで と打つ）と、読みが一致しないので照合しない。

#### 4.2 学習への反映: 猶予に乗る訂正

- `StagedCommit` に次の 2 つを足す。
  - `kind: Learn | RejectOnly`
  - `correction: Option<StagedCorrection { origin_left, rejected: 最大 2 件の (表記, 右ID) }>`
- **`record_learning`:**
  - 学習対象: `stage(owner, r, Y, left, right, correction)`。
  - 学習対象外（カナ変換、raw repair、synthetic exact）で訂正あり: `RejectOnly` の記録を `stage` する。今の `confirm_staged` と同じく、前の記録は学習される。
  - 学習対象外で訂正なし: 今どおり `confirm_staged`。
- **記録を適用するとき（`learn_staged`）:**
  1. `correction` があれば、却下表記ごとに tombstone (r, origin_left, 表記, 右ID) を記録する（メモリとログ）。
  2. `kind == Learn` なら、今どおり (r, Y) を左文脈 left で学習する。
  3. `kind == Learn` で、`correction` があり `origin_left != left` なら、(r, Y) を左文脈 origin_left でも通常の確定レコードとして学習する。general と予測の頻度も 1 増える。予備計測では、exact だけに記録する方式（改善 22 / 悪化 2）より正味の改善が大きかった（改善 32 / 悪化 5）。
  - この順序なので、Y の学習の `sequence` は必ず tombstone より新しい。
- **猶予の結果ごとの扱い:** どの経路でも、適用は記録全体に対して一度だけ行う。

| 猶予の結果 | 扱い |
| --- | --- |
| 次の確定（`stage`）、idle の非 Backspace、`confirm_staged`、`confirm_all_staged` | 適用する |
| 満杯による押し出し | 適用する（#275 と同じく、押し出しは残った扱い） |
| 未処理の idle Backspace（`discard_staged`） | 捨てる |
| 取り消し成功、または結果不明 | 捨てる |
| 取り消し待ち（`hold_staged` / `release_staged`） | 今どおり保留する |
| 接続の reset | 今どおり（取り消し待ちは捨て、それ以外は適用） |

- Y が猶予中に消された場合、tombstone も追加の学習も残らない。ただし trail の Y の項目は消えた確定のスタックに移り、L と却下表記を引き継いでいる。そのため次の打ち直しが Y2 なら X と Y を却下する訂正になり、同じ Y なら X を却下する訂正になる。

#### 4.3 順位付け: tombstone の意味

- **保存場所:** `State` に専用の `RejectionTable` を置く。`Index` の slot には入れない。
  - 構成は 4-way の集合連想表で 2,048 slot。
  - キーは (読みのハッシュ・長さ, L, 表記のハッシュ・長さ)、値は `sequence`。
  - 同じ表に (r, L) ごとの「最新の tombstone」marker を 1 件置く。表記の長さを `u16::MAX` にした予約キーで、表記に依存しない。
  - 満杯の bucket では `sequence` が最小の項目を上書きする。上書きされた tombstone は、ないのと同じになる。X の exact の学習は適用時に消えているので、戻るのは位置ゲート付きの general だけである。marker が上書きされると、その (r, L) の tombstone はすべて効かなくなる（`preference` は marker がある (r, L) でだけ tombstone を引くため）。2,048 slot が埋まるのは、数百回以上の訂正を重ねた後である。
- **適用（実行中とログ再生で同じ処理）:**
  1. `sequence` を 1 進める。実行中は `State::sequence`、再生では `prepare_learning` が記録ごとに進める値で、どちらも通常の確定と同じ数え方である。
  2. `Index` の exact slot (左 L, 右 R_X, r, X) を消す。
  3. `RejectionTable` に (r, L, X) をこの `sequence` で登録する。
  4. marker (r, L) の `sequence` をこの値に更新する。
- **X を (r, L) で再び学習したとき:** (r, L, X) を `RejectionTable` から外す。marker は残す。
- **`preference(r, L, candidates)` の変更:**
  1. marker (r, L) を 1 回引く。marker がなければ処理は今と同じ。
  2. exact と general の両方の検索で、(r, L, 表記) の tombstone がある候補を飛ばす。tombstone は右ID を区別しないので、X が L で別の右ID の exact の学習を持っていても、その学習は競合の検出にも順位にも使わない。
  3. exact の検索: 提示された候補のうち、exact の学習の `sequence` が marker より新しいものがあれば、最新のものが位置ゲートを無視して勝つ。この場合は `exact_conflict` を立て、general の上書き抑止（`superseding_exact`）も今の競合時と同じに働かせる。
  4. marker より新しい学習がなければ、今の規則（頻度、位置ゲート、段階 1 の競合規則）をそのまま使う。

- **代表的な場面:**

| 場面 | 結果 |
| --- | --- |
| X が exact 8 件。訂正 Y（辞書の index 3、学習対象） | X の exact は消える。Y は exact 1 件だが marker より新しいので勝つ |
| X が exact 8 件と general 8。カナ変換で訂正 | X の exact は消え、L では general も飛ばす。辞書順（メンション）になる |
| L に古い Z（exact 1 件、index 4）があり、X をカナ変換で訂正 | marker より新しい学習がないので、Z は Weak の位置ゲート（index 1 まで）で選ばれない。辞書順になる |
| tombstone の後に L で X を選び直す | X の tombstone が外れる。X は marker より新しいので X が勝つ |
| 別の文脈 L2 での X | 影響なし |

- **計算量:** marker の探索は `preference` 1 回につき 1 回。候補ごとの tombstone の探索は、marker がある (r, L) でだけ、exact と general の検索で 1 回ずつ増える。どれも固定の bucket を引く定数時間の処理で、割り当てはない（`best_candidate` の既存の契約を守る）。

#### 4.4 永続化

- **tombstone の記録:** format 3 の `RECORD_COMMIT` を使い、各フィールドを次のようにする。
  - reading: 空文字列。
  - surface: `U+E001` + r + `U+E002` + X。
  - left_context: L。
  - right_context: R_X。
  - day: 記録した日。
  - r か X が空のとき、どちらかに `U+E001` / `U+E002` が含まれるとき、長さが上限を超えるときは記録しない（メモリ上の適用もしない）。
- **新しいエンジン:** `ReplayView` が空の読みと接頭辞からこの記録を判別し、`ReplayEvent::Reject` として返す。形の崩れた番兵（区切りがない、部分が空）は再生でも無視する。
- **旧エンジン（2.0.15 以前、format 3）:** コードで確認した影響は次のとおり。
  - `decode_record` は空の読みを受け付ける（`crates/sakura-store/src/learning/codec.rs` 69-133 行）。
  - `prepare_learning` は通常の確定として再生する。その際の影響は次の 3 点。
    - `Index::learn` は空の読みのキーで slot を 2 つ使う。その bucket の既存 slot を最大 2 つ上書きしうるが、変換の読みは空にならないので照合されない。
    - `PredictionHistory::learn` は空の読みを捨てる（learning.rs 516 行）ので、予測候補には出ない。
    - `best_candidate` は提示された候補の表記と読みでしか照合しないので、順位に影響しない。
  - 旧版の `learning show/export` には、読みが空の行として現れる。旧版の `forget_exact` では消せないが、`learning clear` では消える。
- **compaction:** `compact_for_open`（`crates/sakura-store/src/learning/log.rs` 1156-1190 行）は、新しい側の記録を順序どおり残す。
  - tombstone が落ちるときは、それより古い X の記録も落ちている。
  - 残った記録を再生すると、表の上書きを除いて同じ状態になる。
- **状態の作り直し:** ログを再生して状態を置き換える箇所は 4 つある（起動の `open`、compaction の `maintain`、個別削除の `forget_prediction_exact`、`clear`。テスト用の `compact_state` も同じ）。`PreparedLearning` に `RejectionTable` を加え、この 4 か所で `Index` などと一緒に置き換える。
- **個別削除:** `forget_exact(r, X)`（log.rs 280 行、323 行の条件）を拡張し、復号した (r, 表記) が (r, X) と一致する tombstone も消す。予測候補の削除（Ctrl+Delete）で X を忘れると、X の tombstone も残らない。
  - `forget_prediction_exact` は今、(r, X) と同じ猶予中の記録を捨てている（1095-1100 行）。これに加えて、猶予中の訂正の却下表記からも (r, X) を外す。外さないと、削除の後に記録が適用されて X の tombstone ができてしまう。
  - 削除を指示したセッションの trail からも (r, X) を外す。ほかのセッションの trail は外さない。残った却下は、そのセッションで同じ読みを消して打ち直したときだけ効き、そのときは X をもう一度却下したのと同じ意味になる。
- **スナップショット:** `read_snapshot`（log.rs 563-600 行）は tombstone を `records` から除き、新しい `rejections` に入れる。形の崩れた番兵はどちらにも入れない。
  - 設定の `learning export` と `learning show` は、`rejections` を `#reject` で始まる行として、既存の表の後に出力する。既存の列と行は変えない。
  - リポジトリ内に学習 TSV を読むコードはない。読み手は、今も先頭にある `# sakura-learning-format` 行と同じく、`#` で始まる行をコメントとして読み飛ばせばよい。
- **プライバシー:** tombstone が保存するのは、通常の確定記録と同じ種類の情報（読みと表記）である。保存場所も同じ学習ログで、`learning clear` で消える。新しい種類の個人データは増えない。

#### 4.5 ロールバック

前のバージョンに戻しても、tombstone は 4.4 のとおり順位と予測に影響しない。前の版で学習ログを消す必要はない。

### 5. 評価（再生ツールとリリース判定）

- **ツール:** `tools/conversion-eval` に bin `learning-replay` を追加する。
  - 入力: 入力履歴の平文 export、`learning export`、同梱辞書。
  - 本物のコードを使う範囲: エンジンの feature `replay-eval` だけで公開する再生用 API を通じて、次を使う。
    - 訂正の検出: `CorrectionTrail`。
    - 学習と順位付け: `LearningService`（位置ゲート、競合規則、tombstone を含む）。
    - 日付は明示的に渡す。
  - 候補の並び: `sakura_core::Converter` で、同梱辞書から作る（`neural-eval` の baseline と同じ方法）。
  - 再生していないもの: #275 の猶予のタイミング（規則に従って近似する）、確定キャッシュ、reranker。
- **予備計測との関係:** 1 章・4.2・7 章の数値（改善 22 / 悪化 6、147 回と 128 回、243 件のうち 3 件、改善 32 / 悪化 5）は、設計時にローカルの一時スクリプトで計測した。そのスクリプトはリポジトリに残していない（入力内容を含むため削除した）。採用する方式の判定は、このツールの出力だけで行う。ツールは次の数も出し、予備計測の 147 回と 128 回を同じ実データで再計測する。
  - 訂正の数。
  - 同じ表記の打ち直しの数。
  - 結果は 5 章の末尾に追記する。採らなかった方式（general も消す、exact だけに記録する）の数値は、予備計測のまま残す。
- **出力:** 件数だけを出す。`--examples` を付けたときだけ表記の組を表示し、公開しない。
- **忠実度の確認:** engine 2.0.13 以後の変換について、ツールの baseline が exact の学習で選ぶ先頭候補を数える。それが、履歴に記録された実際の先頭候補（変換直後の preedit）と一致する割合を報告する。
- **判定:** 方式ごとに、学習が先頭に選ぶ表記がユーザーの最終選択と一致するかを比べる。最終選択は、残した表記か、消した後に打ち直した表記。現行と差が出た変換だけを数える。
- **リリース判定:** 次の 2 つを満たさなければリリースしない。
  - 改善 ≥ 2 ×（悪化 + 辞書順に戻った変換のうち元が正解だったもの）。
  - 忠実度 ≥ 80%。満たさない場合は原因を調べ、報告してから判断する。
- **限界:**
  - データは開発者 1 人の 21 日分（訂正 128 回）で、最終選択も同じ人の判断である。
  - リリース後 14 日の新しい履歴で同じ再生を行い、判定を再確認する。満たさなければ前の版に戻す（4.5）。

### 6. テスト

fail-before の対象は、現行コードで失敗し、実装後に通るテストである。どのテストも実際のユーザー状態に触れない（`LearningService::memory()` か一時ディレクトリを使う）。

#### 6.1 dispatch（engine）

- **T1:** 面ション を左文脈 0 で 8 回学習する → 確定 → 全部消す → 同じ読みをカナ変換で確定 → 次の左文脈 0 の変換で 面ション が先頭にならない。カタカナの学習記録も増えていない（fail-before）。
- **T2:** ズミ を左文脈 L で学習する → L で確定 → 全部消す → 打ち直して、辞書の index 2 以降にある候補を選ぶ → 次の L の変換でその候補が先頭になる（fail-before）。
- **T3:** ズミ と で を続けて確定 → 両方消す → 打ち直す → T2 と同じ結果になる（fail-before）。
- **T4:** 消して同じ表記を打ち直す → tombstone も追加の学習もない。
- **T5:** 消去の連続が、矢印、Enter、Delete、取り消しの窓の外の Ctrl+Backspace、Alt+Backspace、`ResetDocumentContext` のそれぞれで途切れる → 照合しない。Shift+Backspace は 1 文字の消去として数え、照合する。
- **T6:** Backspace が 1 文字足りない、または 1 文字多い → 照合しない。
- **T7:** 訂正の確定 Y を猶予中に消す → tombstone も追加の学習もない。続けて Y2 を確定する → X と Y の tombstone ができる。Y2 の代わりに同じ Y を確定する → X の tombstone ができる。
- **T8:** Ctrl+Backspace の取り消しが成功した後にカナ変換で確定する → X の tombstone が L にできる（fail-before）。
- **T9:** 機微な scope の確定は trail に入らない。

#### 6.2 learning（engine）

- **L1:** tombstone の後の Y（exact 1 件、index 3）が勝つ。tombstone なしでは位置ゲートで負ける（fail-before）。
- **L2:** カナ変換による tombstone と、古い Z（exact 1 件、index 4）がある → Z は選ばれない。
- **L3:** general の抑止は L だけに効き、L2 では X が今どおり選ばれる。
- **L4:** tombstone の後に X を選び直す → X が勝つ。
- **L5:** `RejectionTable` の bucket を埋めて上書きする → 上書きされた tombstone は効かない。表の大きさは一定のまま。
- **L6:** ログ再生の順序を保つ。X の記録、tombstone、X の記録の順に再生すると、最後の X が有効になる。実行中の状態と、同じログを開き直した状態とで順位が同じになる。compaction の後も同じ順位になる。
- **L7:** `forget_prediction_exact(r, X)` で X の tombstone も消え、再起動後も戻らない。X を却下する訂正が猶予中なら、その記録を後で適用しても X の tombstone はできない。
- **L8:** 猶予の種類ごとの扱い:
  - `RejectOnly` は Y の確定記録を書かない。
  - `discard_staged` で記録は捨てられる。
  - `confirm_all_staged`、押し出し、接続の reset ではちょうど 1 回だけ適用される。
- **L9:** X が L で別の右ID の exact の学習（Strong）を持つ。tombstone の後は、exact の検索でも X は選ばれず、競合の検出にも使われない。

#### 6.3 store

- **S1:** 番兵の符号化と復号が往復する。形の崩れた番兵は無視される。条件に合わない入力は書かない。
- **S2:** `read_snapshot` は tombstone を `records` から除き、`rejections` に入れる。
- **S3（ダウングレード）:** 旧エンジンと同じ処理経路を、変更していない関数でテストする。`Index::learn` と `PredictionHistory::learn` に tombstone のフィールドをそのまま渡し、次を確認する。2 つの関数自体を変更していないことは diff で確認する。
  - (r, X) と (r, Y) の `preference` が変わらない。
  - 予測候補に何も出ない。

#### 6.4 ツールと既存の保証

- **ツール:** 合成した小さな履歴と学習 TSV で、訂正の件数、各判定区分の件数、忠実度の計算が期待どおりになる。
- **既存の保証:** 次をすべて維持する。
  - `a mechanical kana transform must never bias the reading towards katakana`。
  - #273、#275、#277 の猶予テスト。
  - #282（かんじ）。
  - 段階 1 の競合テスト。

### 7. 残るリスク

- **誤照合:** 最初の Backspace の後にマウスでキャレットを動かしたり範囲を選んだりすると、エンジンには見えないため誤照合しうる。最初のキーは TSF が隣接を再確認する。照合には同じ読みの打ち直しと文字数の完全一致が必要なので、影響は次の範囲にとどまる。
  - (r, L) での X の tombstone。
  - L での Y の 1 回の学習。
- **同音異義語の使い分け:** 同じ文脈で同音異義語を使い分けていると、最後の訂正が先頭になる。予備計測の悪化 5 件はすべてこの型だった（変身/返信 など）。
- **評価データ:** 評価データは 1 人分である。リリース判定と、リリース後の再確認（5 章）で補う。

### 8. 実装とリリースの順序

1. store: 番兵記録の符号化と復号、`ReplayEvent::Reject`、`read_snapshot` の `rejections`、`forget_exact` の拡張（S1-S3）。
2. learning: `RejectionTable`、`preference` の変更、猶予の種類と訂正、`prepare_learning`（L1-L9）。
3. correction と dispatch: `CorrectionTrail`、`record_learning` と idle キーと取り消しの接続（T1-T9）。
4. 設定の `learning export` の `#reject` 行。
5. ツール `learning-replay`。実データで判定（5 章）し、結果をこの文書に追記する。
6. CI 一式をローカルで実行 → PR → マージ → 2.0.16 のリリース → 開発機に再インストール → インストール版で `learning show` が動くことと版数を確認する。
