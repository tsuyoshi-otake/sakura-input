# CLAUDE.md preservation map

Commit `9297343` first ran `git mv CLAUDE.md docs/history/CLAUDE.pre-154.md`,
preserving the original 40,118 CRLF bytes and
SHA-256 `0f50a454c692373d4060960e6f3624498979921c7bf51b1bae9928640360613c`.
The compact entry point was installed in the following commit. Do not replace
the moved snapshot with a newline-normalized copy.

| Original top-level section | Canonical destination |
| --- | --- |
| `テスト出力規約` | `docs/decisions/test-output.md` |
| `製品の主目的` | `docs/decisions/product-direction.md` |
| `リリース署名に関するowner判断` | `docs/decisions/release-signing.md` |
| `更新trust stateの扱い` | `docs/decisions/update-trust-state.md` |
| `Issue #58 GPT-5.6 Luna文章変換・選択文字列校正` | `docs/decisions/issue-58-ai-text.md` |
| `タスクバー入力モードasset` | `docs/decisions/issue-26-mode-indicator.md` |
| `Issue #27 Sakura候補ポップアップ` | `docs/decisions/issue-27-candidate-popup.md` |
| `Issue #28 選択候補の辞書詳細` | `docs/decisions/issue-28-candidate-details.md` |
| `Issue #30 重要辞書のSakura作成説明` | `docs/decisions/issue-30-dictionary-details.md` |
| `Issue #32 Sakura-Rerank-Tiny-v1 統合` and subsections | `docs/decisions/issue-32-reranker.md` |
| `最優先タスク：VS Codeで文字入力中に落ちる問題` and subsections | `docs/history/issues/vscode-crash-investigation-20260802.md` |
| `既存の目的（維持必須）` | `docs/decisions/input-behavior.md` |
| `実装済みの重要箇所` | `docs/history/CLAUDE.pre-154.md` |
| `開発者モード：入力・変換履歴` | `docs/decisions/developer-history.md` |
| `検証済みの状態` | `docs/history/CLAUDE.pre-154.md` |
| `別セッションで再開するときの手順` | `docs/history/CLAUDE.pre-154.md` |
| `次に問題が報告された場合の切り分け` | `docs/history/CLAUDE.pre-154.md` |
| `作業上の注意` | `AGENTS.md` |

No original text is deleted: the moved snapshot is the byte-exact authority.
Decision extracts provide direct active guidance, while dated investigations,
old verification, and superseded handoffs remain under `docs/history`.
