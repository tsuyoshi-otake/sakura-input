## Issue #446 ユーザー辞書の完全一致を「辞書がすでに答えている」証拠として扱う（実装済み、2026-11-08）

- オーナーは2026-11-08に「君の推奨で頼むよ。ATOKに似せた仕様が良いかな」と決めた。読み全体に一致するユーザー辞書の語（`CandidateEvidence::ExactUser`）は、システム辞書の完全一致（`ExactSystem`）と同じく「辞書がすでにこの読みに答えている」証拠として扱う。ユーザーが自分で登録した語は、推測で作った候補より優先する。
- この証拠を使う判定は3か所あり、どれも同じ扱いにそろえた。
  - F2 `synthesis/dates.rs` `demote_generated_day_suffixes`：生成した日付の接尾辞（例：動詞語幹＋「八日」）を、システム辞書だけで読み全体を覆う経路**または**読み全体のユーザー語より後ろへ下げる。消しはしない。
  - F3 `ranking/quality_gate.rs` `apply_exact_lexical_quality_gate`：読み全体の完全一致があるときは、未確認の修復候補（濁点の付け外しなど）と、コストの高い細切れの組み合わせを外す。完全一致の判定に `ExactUser` を加えた。見た目の条件（2文字以上の漢字だけ、またはカタカナ・英数字だけ）と、読みの長さの条件（6文字以下、または分割できない語）はシステム辞書の語と同じ。
  - F4 `search/lattice.rs` `add_local_repair_edges`：打った区間そのものが辞書の読みなら、その区間の修復に `EXACT_SPAN_REPAIR_PENALTY` を足す。ユーザー辞書の語は修復の辺より後に格子へ入るため、呼び出し側がその区間にユーザー語の辺が入るかを、同じ受け入れ条件（英数字の塊の途中から始まる・途中で終わる語は入れない、単文節変換では読み全体だけ）で答える。
- 変えないこと：ユーザー語のコスト、`evidence.rs` の `neural_group` の順位（ユーザー語はシステム語より上。既存のまま）、ユーザー語が無いときの修復候補と日付候補。精度ゲートのコーパスはユーザー辞書を使わないので、結果は変わらない（2026-11-08、変化0件）。
- 範囲外：#448（ひらがなが1位の短い読みで読みを飛ばす規則）。F5〜F7 は下の節。
- テスト：`crates/sakura-core/src/conversion/conversion_tests.rs` の `user_word_for_the_whole_reading_suppresses_unconfirmed_repairs_and_costly_mosaics`（F3）、`user_word_for_a_typed_span_is_exact_span_evidence_for_a_repair`（F4、複数文節）、`single_segment_user_word_is_exact_span_evidence_for_a_repair`（F4、単文節）、`crates/dictc-core/tests/conversion.rs` の `generated_day_suffix_stays_behind_a_user_word_for_the_whole_reading`（F2。接続コストが要るため接続行列つきの辞書で組む）。4本とも修正前のコードで失敗する。

## F5〜F7 の結論（2026-11-09）

- オーナーは2026-11-08に #446 の判断を「君の推奨で頼むよ」と任せ、2026-11-09に F5 以降へ進めることを了承した。Claude が各項目の挙動を調べ、F6 を直し、F5 と F7 は直さないと決めた。
- F5 `search/lattice.rs` `admits_system_boundary`：直さない。
  - 境界の別名（`BOUNDARY_ALIAS`。いまは がわ→川 の一般名詞の別名だけ）を読みの途中で許す例外は、#306（`93a7a71`）で入れた。目的は、読み全体がシステム辞書の語である読み（石川、市川など）の辺を #306 の前と同じに保つことで、回帰を避けるための例外である。「辞書がすでに答えている」証拠で候補を守る判定ではない。
  - ユーザー語にもこの例外を許すと、ユーザー語の読みの途中に安い 川 の辺が戻り、#306 で直した「〜側」が「〜川」になる組み合わせの候補がまた作られる。ユーザー語そのものは今も候補に残る。
  - この挙動は `issue_306_user_word_does_not_grant_system_exact_reading_exception` で固定してある。
- F6 `conversion/mod.rs` `build_exact_top1_lattice`：直した。
  - 英字と数字を両方含む ASCII の識別子（例：esp32）を Shift で打ったときの経路。完全一致の辺の上限（候補数−1。読みが4文字以下なら255、8文字以下なら107、それより長いと17）をシステム辞書の語から先に使っていた。そのため、システム辞書の語だけで上限に達すると、同じ読みのユーザー語が候補から消えていた。
  - いまの辞書では起きない。2026-11-09に `data/it-terms.tsv` で数えると、英字と数字を両方含む読みで語が最も多いのは10語（上限255）、9文字以上の読みでは最も多くて2語（上限17）だった。ただし #439 の F1 と同じ「ユーザー語が消える」型なので、データが増えて起きる前に直した。
  - 直し方：ユーザー語を先に入れ、残りの枠をシステム辞書の語で埋める。上限に届かない読みでは、入る辺はこれまでと同じ。候補の並びもこれまでどおりコスト順。
  - テスト：`crates/sakura-core/src/conversion/conversion_tests.rs` の `exact_top1_keeps_the_user_word_when_system_words_fill_the_edge_limit`。修正前のコードで失敗する。
- F7 `sakura-engine` `prediction.rs`：直さない。
  - 予測では、ユーザー辞書の語は読みの前方一致（`predictive_search`）で直接の候補に入る。前方一致には読みが同じ語も含まれるので、読み全体に一致するユーザー語は、システム辞書だけを見る補充（`fill_with_exact_readings`）がなくても候補に入る。
  - ユーザー語のコストは品詞の既定値（最大2,500）から `USER_DICTIONARY_BONUS`（3,000）を引いた値で、読みが同じなら0未満になる。システム辞書の直接の候補は0以上なので、読み全体に一致するユーザー語はその上に来る。修復の候補は直接の候補の後ろにしか入らないので、ユーザー語より前には出ない（`crates/sakura-engine/src/prediction_tests.rs` の `direct_user_provenance_wins_over_a_system_repair` で固定してある）。
  - 残る違い：読み全体に一致するシステム辞書の語が予測用でない行（`PREDICTION` なし）のときだけ、修復の候補を出さない（`query_has_exact_non_predictive_reading`）。その語は直接の候補に入らず最後の補充でしか出ないので、修復の候補に先を越されないための抑制である。ユーザー語は直接の候補に入るのでこの理由が当てはまらず、予測用のシステム辞書の語と同じく、空いた枠に修復の候補が入る。ユーザー語の順位は変わらない。
