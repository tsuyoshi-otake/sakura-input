# Sakura Input 1.0.34

Sakura update-signing v2で配布する最初の通常自動更新です（#90）。v1.0.33のupdaterは、GitHub Releaseから取得したmanifestをSakura Input固有の固定公開鍵で検証してから、このinstallerを実行します。

## 主な変更

- release sequenceを2へ進め、rollback／replay防止を有効にした実運用更新にしました。
- `release-manifest-v2.txt`とdetached `release-manifest-v2.sig`を正式Releaseの必須assetとして配布します。
- installerのsize、SHA-256、repository、tag、source commit、期限、minimum updater version、Authenticode policyを署名前のcanonical manifestへ固定します。
- 公開前後にGitHub artifact attestationと3 assetの再ダウンロード検証を行います。

## 更新方法

v1.0.33では、設定画面の更新確認または次のコマンドから更新できます。

```text
sakura_settings.exe update apply
```

v1.0.32以前はSakura update-signing v2の公開鍵を持たないため、先にv1.0.33を一度だけ手動インストールしてください。

## Authenticodeについて

このReleaseはowner承認済みのAuthenticode未署名版です。Windowsのダイアログには引き続き「不明な発行元」と表示されます。有償証明書は使いません。

未署名installerを無条件には許可しません。Sakura manifest署名が正しく、manifestが`authenticode=unsigned`を宣言し、`WinVerifyTrust`が正確に`TRUST_E_NOSIGNATURE`を返した場合だけ更新を続行します。不正署名、失効署名、未知provider、policy不一致はfail closedで拒否します。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。
