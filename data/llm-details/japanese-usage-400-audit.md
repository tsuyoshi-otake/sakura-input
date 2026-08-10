# 日本語の使い分け400語の辞書照合監査

入力は `japanese-usage-400-candidates.md` に対応する400行のJSONL。入力attachmentはrelease
artifactではない。

## 結果

- records: 400
- comparison groups: 200
- unique numbers: 400
- unique `(surface, reading)` pairs: 400
- malformed JSON: 0
- 相互comparison/relation不整合: 0
- semantic `ready`: 388
- semantic `needs_review`: 12
- 現行14カテゴリ辞書とのexact pair一致: 399
- 現行detail coverageとのexact pair一致: 74
- 現行辞書にexact pairがないもの: 1（`根拠資料` / `こんきょしりょう`）
- relation status: 全400件 `research_only_unverified`

## 判定

既存399件へ新しいentryを作らない。既存detailを持つ74件は、LLMの新規detail laneではpair単位で
抑止する。残る既存entryはexact identityを再取得してdetail target候補にする。`根拠資料`は
semantic `ready`だが、entry追加、品詞接続、生成後identity固定を先に行う。

12件の`needs_review`は、多義、読みの揺れ、領域依存を理由に自動昇格しない。400件のrelationsは
比較相手との相互リンクとして構造上は整合するが、まだ調査結果であり、独立した語義・関係審査を
通るまでreleaseへ入れない。
