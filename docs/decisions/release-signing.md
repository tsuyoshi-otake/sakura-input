## リリース署名に関するowner判断（2026-08-22）

- ownerは、コード署名証明書が未設定でもSakura Inputの正式リリースを公開してよいと明示承認した。GitHub Actionsの`release` environmentに署名secretがないことを、リリースのblockerにしない。
- 未署名のinstallerを署名済みと表現してはいけない。リリースノートと導入案内にはAuthenticode未署名であること、GitHub ReleaseのSHA-256を照合して手動インストールすることを明記する。
- updater側の`WinVerifyTrust` fail-closed検証は弱めない。
- 【2026-09-15訂正】以前ここには「未署名リリースの自動取得・実行は拒否する」と書いていた。しかし#90（update-signing v2）以降の契約と実装はそうなっていない。
  - 自動更新が未署名installerを受け入れるのは、次の3条件がすべて揃う場合だけである。
    1. 固定公開鍵で署名されたmanifestが`authenticode=unsigned`を宣言している。
    2. installerのサイズとSHA-256がmanifestと一致する。
    3. `WinVerifyTrust`が正確に`TRUST_E_NOSIGNATURE`を返す。
  - 次の場合は拒否する。
    - 不正な署名、失効した署名、未知のprovider
    - manifestが`required`なのにinstallerが未署名
    - manifestが`unsigned`なのに有効なAuthenticode署名が付いている
  - 正の仕様は`verification/update-signing-v2.md`の判定表と`crates/sakura-settings/src/updater.rs`にある。
  - リリースノートで「未署名版は自動更新されない」と書いてはいけない。
- 署名secretが3点すべて揃っている場合は従来どおり署名・検証してよい。部分設定は曖昧な成果物を作らずCIを失敗させる。
