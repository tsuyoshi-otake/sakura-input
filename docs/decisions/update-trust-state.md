## 更新trust stateの扱い（Issue #150、2026-09-09）

- `%LOCALAPPDATA%\SakuraInput\update\trust-state.txt`のように、ユーザーが削除できる場所に置いた記録が、形式は正しいが今の実行バイナリにとって使えないだけのとき、それを**「存在しない」場合より厳しく失敗させてはいけない**（形式そのものが壊れている場合は別扱いで、下記のとおり終端してよい）。削除すれば消える情報を根拠に恒久エラーを返しても攻撃者は止まらず、正規の利用者だけが更新確認を永久に失う。1.0.39の`更新情報の検証に失敗しました: update trust state is below the embedded trust floor`はこの型の欠陥だった。
- anti-rollback境界は2層ある。**動かせない**下限は、バイナリへ`include_bytes!`で埋め込んだfloor（`data/update-signing/release-sequence.txt`）だけで、バイナリを差し替えない限り変えられない。永続化したstateはその上に載る**追加の**replay境界であり、使える場合にだけ効く（`authorize_manifest`は`previous.highest_sequence`を下回るmanifestを拒否し続ける）。埋め込みfloorを下回るstateや`EMBEDDED_TRUST_EPOCH`と一致しないstateは、この追加境界として使えないので「境界が無い」と解釈し、署名検証済みmanifestから書き直す。エラーにしない。埋め込みfloorはそのまま効いているので、下限が消えるわけではない。
- 手動インストールはtrust stateを進めない。updater経由の更新確認だけが書き込む。したがって「stateのsequence ＜ 実行中ビルドのfloor」は異常ではなく、リリースのたびにfloorが上がる以上、手動インストールを続ける利用者では必ず発生する。この差を異常として扱う分岐を新設しない。
- 形式不正・上限超過のstateは従来どおりfail closedで終端してよいが、メッセージに**そのファイルのフルパスを含める**。利用者が自力で回復できない終端エラーを作らない。
- replay／equivocation／rollbackの拒否は弱めない。使えないstateを「無し」として扱うことと、正当なstateによる下限判定を外すことは別物である。`verification/update-signing-v2.md`のv2契約はtrust stateに言及していないため、この回復挙動は契約変更ではない。
- 一般化：更新・署名まわりで「埋め込み値」と「永続化した値」を比較する分岐を足すときは、永続化側が欠落した場合と古い場合の two case を必ず並べて設計し、古い側が欠落側より厳しい結果にならないことをテストで固定する。

