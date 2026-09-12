## リリース署名に関するowner判断（2026-08-22）

- ownerは、コード署名証明書が未設定でもSakura Inputの正式リリースを公開してよいと明示承認した。GitHub Actionsの`release` environmentに署名secretがないことを、リリースのblockerにしない。
- 未署名のinstallerを署名済みと表現してはいけない。リリースノートと導入案内にはAuthenticode未署名であること、GitHub ReleaseのSHA-256を照合して手動インストールすることを明記する。
- updater側の`WinVerifyTrust` fail-closed検証は弱めない。未署名リリースの自動取得・実行は拒否される設計を維持し、未署名版は手動インストール対象として扱う。
- 署名secretが3点すべて揃っている場合は従来どおり署名・検証してよい。部分設定は曖昧な成果物を作らずCIを失敗させる。
