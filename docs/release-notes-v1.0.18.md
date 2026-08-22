# Sakura Input 1.0.18

候補操作、変換品質、入力状態の安全性、評価基盤、設定画面、更新確認、インストーラーをまとめて改善したリリースです。

## 主な変更

- 入力中に `Tab` を押したとき、表示が変わらない同一文字列の先頭候補を飛ばして、最初の実用候補へ進むようにしました。候補行は左クリックでも確定できます。クリックは表示revision、候補index、所有session、フォーカス、入力欄、入力スコープを再検証してから通常のTSF edit sessionで適用し、古い候補や別の入力欄へは反映しません。（#81）
- 予測・変換・学習候補の順位付けと状態遷移を整理し、裸の数詞候補、日付・助数詞の生成候補、辞書にある全体一致候補、誤入力補正候補、重要語、候補距離が互いを不当に押しのけないようにしました。たとえば `いちにち` は辞書の `一日` を先頭に保ちながら `1日` も選べます。履歴由来候補を確定・変換する経路でも、同じ文字列の重複適用やstaleな結果を抑止します。（#71、#78、#80）
- 高リスク同音異義語へexact-entryの意味と関係語を追加し、compoundと同形異義語を別entryとして検証する辞書テストと審査記録を追加しました。曖昧なentry、identity不一致、未審査語は引き続きfail closedで説明なしになります。（#79）
- Shift開始の英字入力、Space／変換、Backspace、focus変更、Dual TSF、候補UI境界の状態機械と回帰テストを強化しました。内部状態がcompositionより長く残る経路、hostとの二重適用、失効callback、失敗後の曖昧な中間状態を明示的な終端へ収束させます。
- 機械的contract、実engine capture、blind Judge、calibration、baseline、品質gateを分離した `ime-eval` 基盤を追加しました。入力履歴から承認した匿名fixture、英数字literal、混在romaji、同音異義語、変換品質Stage 1をversioned corpusとして扱い、生成レポートはコミットしません。（#66、#73）
- 入力履歴のリセット、統計確認、診断トレース確認を設定CLIから実行できるようにしました。設定画面は同時に複数起動せず、既存画面を前面へ戻します。
- 設定画面の起動時にGitHub Releasesの更新を確認します。更新が利用可能な場合はダイアログで確認し、同意した場合だけ取得・SHA-256検証・Authenticode署名検証・インストールを行います。更新確認は設定画面または `sakura_settings.exe update disable` で無効化できます。
- 設定画面、インストーラー、アンインストール表示へSakura Input独自の赤系アイコンを追加しました。インストーラーはSakura Editor NEXTと同じmodern dark wizard、edge-to-edgeのブランド画像、legacy folder artworkを使わない本文配置、high-contrastを尊重する桜色progressへ揃え、日本語／英語の案内文も見直しました。（#82）

## 検証

リリースworkflowはformat、strict clippy、workspace test、残存test process、version整合性、辞書再現ビルド、release binary、installer warningsを検査します。installerからupdater manifestを生成し、サイズとSHA-256が一致することを確認します。署名secretが完全に設定されている場合はAuthenticode署名も検証しますが、1.0.18はowner承認により未署名で公開します。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの `sakura_setup.exe` はAuthenticode未署名です。GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してから手動で実行してください。未署名installerは自動更新の署名検証を通らないため、1.0.18への更新にはReleaseページからの手動インストールが必要です。

アップグレードは versioned payload の side-by-side 切替で行われます。新しい payload の検証に失敗した場合は、TSF 登録を新しい版へ切り替えずに中止します。
