## アーキテクチャ一連変更の製品リリースはメジャー上げ（owner判断、2026-09-13）

- いま進んでいるアーキテクチャ再編（Phase 0–7 の実装を製品として出すとき）の正式リリース番号は、現行 `1.0.39` のパッチ上げではなく **`2.0.0`** とする。
- 中間の実装 PR では `Cargo.toml` の workspace version を上げない。番号変更は最終リリース工程だけで行う。
- 最終リリース工程では、少なくとも workspace version、release notes、installer、git tag、update-signing の sequence／manifest を `2.0.0` に揃える。未署名公開の方針と updater の fail-closed 検証は `docs/decisions/release-signing.md` のまま弱めない。
- `1.0.40` として出してはいけない。
