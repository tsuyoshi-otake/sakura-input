# 丁寧語・敬語・メール／チャット定型句400件の辞書照合監査

入力は `polite-business-400-candidates.md` に対応する400行のJSONL。入力ファイル自体は
Codex attachmentであり、release artifactではない。

## 結果

- JSONL records: 400
- unique numbers: 400
- unique `(surface, reading)` pairs: 400
- malformed JSON: 0
- semantic `ready`: 376
- semantic `needs_review`: 24
- relationを1件以上持つrecord: 0
- 現行14カテゴリ辞書とのexact pair一致: 16
- 現行detail coverageとのexact pair一致: 0
- 現行辞書にexact pairがないもの: 384
- 上記384件のうちsemantic `ready`: 366
- 上記384件のうち保留: 18

exact pair 16件のカテゴリ出現数は `01_文法・機能語` 3、`02_活用語` 13、
`03_一般語` 2。ひとつのpairが複数カテゴリに存在し得るため、この合計は16を超える。

## 判定

既存entry 16件は新しいentryを作らず、exact identityを確認してdetail候補にする。未登録384件の
うちsemantic `ready`の366件は、新規entry候補として扱える。`needs_review`の18件は追加しない。

ただし、入力のrelationsは400件すべて空である。definitionの一次調査としては利用できるが、
関連表現・安全な言い換え・応答関係まで完成したデータとは扱わない。relationsを追加する場合も、
現行schemaの `aliases` / `related` / `similar` / `antonyms` だけを使い、丁寧度や利用上の注意を
型がないままrelationへ偽装しない。反対語や関連語を件数合わせで生成しない。

## 次工程

1. 既存16件はexact dictionary identityを固定する。
2. 新規候補366件は配置カテゴリと品詞接続を検証してentryを生成する。
3. 新しい辞書imageからtarget manifestを再生成する。
4. definitionをdraft schemaへ変換する。
5. relationsは明示的に検証できた語だけ追加する。
6. 独立審査後にrelease schemaへ昇格する。
