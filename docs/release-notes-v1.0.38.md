# Sakura Input 1.0.38

変換候補の表示上のidentityを全操作で統一し、候補の重複、数字キーによる意図しない候補選択、修復候補の学習による自己強化を防ぎます。設定画面は640×480のコンパクトな外形と標準タブへ再設計しました（#108、#144）。

## 主な変更

- Unicode正規化後に同じ表記となる候補を一つのvisible projectionへまとめ、表示、番号選択、click、UI Automation、辞書detail、確定が同じ候補identityを参照するようにしました。
- 変換候補に未フォーカス／フォーカスの状態を持たせ、候補を選択する前の数字入力を候補番号として誤消費しないようにしました。
- 候補の生成根拠を保持し、Advanced repairや弱い生成候補が、信頼できるwhole-reading exact候補を学習経由で暗黙に越える経路を抑止しました。
- 設定画面を640×480論理pxへ縮小し、独立ボタン型だった上部ナビゲーションを標準の `SysTabControl32` に変更しました。全19フォームの設定項目を上へ詰め、長いページだけをスクロールさせ、下部の操作ボタンを固定しています。
- 設定画面のLight／Dark、100／125／150／200% DPI、キーボード操作、UI Automation、設定保存と再読込をUser32テストで確認しました。

## 検証と残る課題

format、workspace全テスト、設定画面の実User32テスト、Releaseビルド、辞書の決定性、installer生成を確認してリリースします。実Windows High Contrast、OSの文字拡大、実モニター間のDPI移動は未確認であり、#144で継続して追跡します。候補品質の実履歴による継続観測は#108で追跡します。

VS Codeクラッシュの原因特定や完全な解消を示すリリースではありません。

## 導入方法

このReleaseはowner承認済みのAuthenticode未署名版です。Windowsには「不明な発行元」と表示されます。GitHub Releaseから取得した `sakura_setup.exe` のSHA-256を `release-manifest-v2.txt` と照合して、手動インストールしてください。Sakura固有のmanifest署名は、Windowsのコード署名とは異なります。未署名版は自動更新で取得・実行されません。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。
