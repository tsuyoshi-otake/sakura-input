## Issue #30 重要辞書のSakura作成説明

- 2026-10-27の所有者指示: 活用形・人名・記号を含む全辞書エントリの説明100%を長期目標とし、変換速度と候補順位を検証しながら段階的に進める。件数を埋めるために不確かな意味を推測せず、説明の根拠と各エントリへの対応を確認する。複合候補への表示はエントリ被覆率とは別の問題であり、既存の厳密照合を維持する。
- `data/llm-detail-targets/<batch>`のcommitted target manifestが全入力hashとexact dictionary identityを固定し、`data/llm-details/releases/<batch>`のrelease manifestが審査済みJSONLを固定する。draftは直接importできず、release directoryと対応target directoryを両方指定しない限り`dictc`へ入らない。
- 現在の通常ビルド対象は000010。既定辞書だけから作った242 targetsのうち236語を承認、6語（始め、監督、命令、告知、提言、標記）を保留し、承認語は246 exact-entry detailsとして入る。候補段階の「終わり」は多義・複数identityのためtarget作成前に保留した。承認レコードは全件に少なくとも1つの型付き関係語を持つ。レビューはユーザー指定によりsubagentを使わず、同一モデルの別prompt工程で実施したもので、独立モデル審査とは表現しない。000004以前のreleaseは履歴として残るが通常ビルドへ重ねてimportしない。既存detailと同じNFC正規化済み(surface, reading) pair、曖昧語義、辞書identity不一致、改ざん、未知schemaはfail closedで除外する。
