# 故事・ことわざ・慣用句200件の辞書照合監査

入力は `proverbs-idioms-200-candidates.md` に対応する200行のJSONL。入力ファイル自体は
Codex attachmentであり、release artifactではない。

## 結果

- JSONL records: 200
- unique numbers: 200
- unique `(surface, reading)` pairs: 200
- malformed JSON: 0
- semantic `ready`: 187
- semantic `needs_review`: 11
- semantic `duplicate`: 2
- 現行14カテゴリ辞書とのexact pair一致: 73
- 現行detail coverageとのexact pair一致: 4
- 全カテゴリ・既存detail適用後のsafe targetとのexact pair一致: 34
- 現行辞書にexact pairがないもの: 127
- 上記127件のうちsemantic `ready`で新規見出しとして採用可能: 115
- 上記127件のうち保留: 12（`needs_review` 10、表記重複 2）

safe target 34件だけを `../llm-detail-targets/000006.allowlist.tsv` に移した。allowlistに
入らなかった166件を数合わせでdraft化しない。73件と34件の差には既存detailおよびselectorの
安全除外が含まれるため、理由を推測してreleaseへ戻さない。

ユーザー指示により、現行辞書にexact pairがなくsemantic `ready`の115件は、新しい辞書見出し
として追加対象にする。読みまたは語義が未確定の10件と、表記重複の2件は追加しない。追加後に
新しいentry identityを生成してからdetail targetを抽出するため、既存entryのidentityを流用しない。

## 次工程の条件

`000006.allowlist.tsv` はまだcommitted target manifestではない。全14カテゴリ入力、現在の
exact detail coverage、curated coverageを使ってtarget extractorを再実行し、34件すべてが
safe targetとして再現された場合だけdraft生成へ進む。説明文はrelease schemaへ直接入れず、
独立した審査と現行identityの再検証を必須とする。
