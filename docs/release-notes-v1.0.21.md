# Sakura Input 1.0.21

単独では不自然な連濁形や、根拠の弱い読み修復が候補欄へ混ざる問題を、語ごとのブラックリストではなく辞書属性と候補経路の共通規則で修正するリリースです。

## 主な変更

- Mozc辞書の品詞と、同じ活用語の清音形が実在するという再現可能な根拠から、語頭では使わない連濁形をビルド時に分類します。これにより`ずかい`では`図解`を先頭に保ち、`使い`、`遣い`、`頭蓋`などの不自然な候補を独立変換と予測から除外します。（#80）
- この分類は複合語内部のedgeを削除しません。`きづかい`→`気遣い`、`こづかい`→`小遣い`、`ことばづかい`→`言葉遣い`といった正しい連濁語は維持します。
- 完全一致の信頼できる辞書語がある場合、規則・高度修復・英語読み修復から生じた根拠の弱い候補を抑制します。強い完全一致がない場合のrepair fallbackと、curatedな綴り訂正は維持します。
- 短い日本語の完全一致語がある場合は、高コストな細切れ経路が最初の候補ページを埋めないよう、既存のcost window内で一般的に整理します。user dictionary、exact learning、lossless fallbackは変更しません。

## 検証

- 実際の出荷辞書を2回ビルドし、byte-for-byteの決定性とSHA-256 `89f7d18e6c35428fdf9d44212f0c2128b001ca48d881fd3c11d7d26c55d75047`を確認しました。
- 実辞書で`ずかい`の先頭が`図解`になり、不自然な候補が消えること、および`つかい`の`使い`／`遣い`と複合語内部の正しい連濁を維持することを確認しました。
- held-out評価はSakura 60/60、Mozc 60/60、IT 30/30、変換p99 1.892 msでした。
- `sakura-core`、`sakura-engine`、workspace全体のテスト、format、diff check、残存test process 0を確認しています。release workflowでも生成直後の実辞書に対して今回の回帰テストを明示実行します。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このReleaseはowner承認済みのAuthenticode未署名版です。GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。

未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。updaterのfail-closed検証は変更していません。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
