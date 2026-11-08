## Issue #446 ユーザー辞書の完全一致を「辞書がすでに答えている」証拠として扱う（実装済み、2026-11-08）

- オーナーは2026-11-08に「君の推奨で頼むよ。ATOKに似せた仕様が良いかな」と決めた。読み全体に一致するユーザー辞書の語（`CandidateEvidence::ExactUser`）は、システム辞書の完全一致（`ExactSystem`）と同じく「辞書がすでにこの読みに答えている」証拠として扱う。ユーザーが自分で登録した語は、推測で作った候補より優先する。
- この証拠を使う判定は3か所あり、どれも同じ扱いにそろえた。
  - F2 `synthesis/dates.rs` `demote_generated_day_suffixes`：生成した日付の接尾辞（例：動詞語幹＋「八日」）を、システム辞書だけで読み全体を覆う経路**または**読み全体のユーザー語より後ろへ下げる。消しはしない。
  - F3 `ranking/quality_gate.rs` `apply_exact_lexical_quality_gate`：読み全体の完全一致があるときは、未確認の修復候補（濁点の付け外しなど）と、コストの高い細切れの組み合わせを外す。完全一致の判定に `ExactUser` を加えた。見た目の条件（2文字以上の漢字だけ、またはカタカナ・英数字だけ）と、読みの長さの条件（6文字以下、または分割できない語）はシステム辞書の語と同じ。
  - F4 `search/lattice.rs` `add_local_repair_edges`：打った区間そのものが辞書の読みなら、その区間の修復に `EXACT_SPAN_REPAIR_PENALTY` を足す。ユーザー辞書の語は修復の辺より後に格子へ入るため、呼び出し側がその区間にユーザー語の辺が入るかを、同じ受け入れ条件（英数字の塊の途中から始まる・途中で終わる語は入れない、単文節変換では読み全体だけ）で答える。
- 変えないこと：ユーザー語のコスト、`evidence.rs` の `neural_group` の順位（ユーザー語はシステム語より上。既存のまま）、ユーザー語が無いときの修復候補と日付候補。精度ゲートのコーパスはユーザー辞書を使わないので、結果は変わらない（2026-11-08、変化0件）。
- 範囲外：#446 の F5〜F7、#448（ひらがなが1位の短い読みで読みを飛ばす規則）。
- テスト：`crates/sakura-core/src/conversion/conversion_tests.rs` の `user_word_for_the_whole_reading_suppresses_unconfirmed_repairs_and_costly_mosaics`（F3）、`user_word_for_a_typed_span_is_exact_span_evidence_for_a_repair`（F4、複数文節）、`single_segment_user_word_is_exact_span_evidence_for_a_repair`（F4、単文節）、`crates/dictc-core/tests/conversion.rs` の `generated_day_suffix_stays_behind_a_user_word_for_the_whole_reading`（F2。接続コストが要るため接続行列つきの辞書で組む）。4本とも修正前のコードで失敗する。
