# Sakura Input 1.0.33

Authenticode証明書を購入しなくても、Sakura Input自身の固定公開鍵でrelease manifestを認証して安全に自動更新できるようにするbridgeリリースです（#90）。秘密鍵はGitHub Actionsへ置かず、ownerのWindowsユーザーにDPAPI保護して保管します。

## 主な変更

### Sakura update-signing v2

updaterはcanonical `release-manifest-v2.txt`とdetached `release-manifest-v2.sig`を、同梱したSakura公開鍵で検証します。署名対象にはrepository、channel、platform、release sequence、version、tag、source commit、asset名とURL、SHA-256、size、Authenticode policy、minimum updater version、期限を含めます。

manifestはUTF-8・BOMなし・LF・固定field順のexact bytesです。未知、欠落、重複、並べ替え、非canonical数値、別repository／tag／asset、期限切れ、未知鍵、署名不一致をfail closedで拒否し、installerをdownloadまたは実行しません。

### rollback／replay防止

同梱したrelease sequenceをfloorにし、受理済みのhighest sequence、version、manifest digestをper-user trust stateへ保存します。古いsequence、同一sequenceの別manifest、sequenceだけ増えた非増加version、壊れたstate、lock／保存失敗は明示的な更新失敗として終了します。

### 未署名installerの厳格な扱い

Sakura manifest署名はすべての更新で必須です。そのうえでmanifestの`authenticode` policyを検証します。

- `required`: `WinVerifyTrust`成功時だけ許可します。
- `unsigned`: `WinVerifyTrust`が正確に`TRUST_E_NOSIGNATURE`を返した場合だけ許可します。
- 無効、失効、未知provider、policyと実ファイルの不一致は拒否します。

これは一般的な「Windows署名検証に失敗したら独自署名へfallbackする」実装ではありません。既存のsize／SHA-256再検証とexact-file guardも維持し、検証済みfile identityをinstaller起動境界まで保持します。

### release工程

GitHub Actionsはinstallerとcanonical manifestをbuildし、artifact attestationを生成します。Sakuraのprivate keyはActions、GitHub secret、repository、artifact、ログへ渡しません。ownerがcandidateとattestationを検証した後、ローカルDPAPI鍵でmanifestへ署名し、draftからdownloadした3 assetを再検証して公開します。

## bridgeについて

v1.0.32以前のupdaterはupdate-signing v2公開鍵を持たないため、v1.0.33を安全に自動導入できません。v1.0.33は一度だけ手動でインストールしてください。v2対応後の次releaseから、未署名installerを含む更新でも固定Sakura公開鍵による認証を必須にした自動更新を利用できます。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このReleaseはowner承認済みのAuthenticode未署名版です。Windowsのダイアログには引き続き「不明な発行元」と表示されます。これは有償のコード署名証明書なしでは解消しません。

初回bridgeではGitHub Releaseから`sakura_setup.exe`、`release-manifest-v2.txt`、`release-manifest-v2.sig`の3 assetを取得し、Releaseに記載するSHA-256とSakura manifest署名を確認してから手動で実行してください。v1.0.33導入後はupdaterが同じ検証を自動で行います。

アップグレードはversioned payloadのside-by-side切替です。新しいpayloadの検証または登録に失敗した場合は、TSF登録を新しい版へ切り替えずに終了します。
