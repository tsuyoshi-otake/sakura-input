# Sakura Input 1.0.20

直前の確定文脈と変換学習が競合した場合の候補選択を修正するリリースです。

## 主な変更

- 別の文脈で同音異義語を選択・学習した後でも、cross-commit再解析で強い文法的連続性が確認できた候補を優先します。（#84）
- 代表的な再現手順である`今日は` → `内科`を選択 → `考慮漏れ` → `ないか`で、直近選択キャッシュや永続学習が`内科`を文法候補より上へ移動しないようにしました。
- 修正は`ないか`や`内科`を判定する個別規則ではありません。再解析で改善されたdirect candidateのprovenanceを使い、学習・キャッシュが選択できる候補集合を一般的に制約します。
- cross-commit evidenceがない通常変換では従来の学習強度と候補選択を維持します。exactユーザー辞書、association無効、機微scope、bounded replayのfail-closed動作も変更しません。

## 検証

- 汎用fixtureでは、`検討`と`確認`の2種類の確定語、学習回数1・2・3・8回を組み合わせ、文脈あり8ケースと文脈なし4ケースを検証しました。
- 実辞書では、画像と同じ直近選択シーケンス、association無効、学習ログ保存後のengine再起動を含む4ケースを検証しました。
- release workflowは辞書ビルド後にIssue #84の実辞書テストを明示実行します。ignored testを通常のworkspace test成功だけで合格扱いにしません。
- formatting、strict clippy、workspace tests、残存test process、`git diff --check`を確認しています。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

署名secretが完全に設定されている場合、release workflowは全配布binaryとinstallerをAuthenticode署名して検証します。署名secretがない場合はowner承認済みの未署名releaseとして公開し、部分設定の場合はreleaseを失敗させます。

未署名で公開された場合は、GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
