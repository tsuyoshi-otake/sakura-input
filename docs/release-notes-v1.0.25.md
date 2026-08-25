# Sakura Input 1.0.25

Sakura Input に内蔵のローカルメモ「Sakura Pad」を追加するリリースです。IME 本体の変換品質、キー操作、辞書は変更していません。Pad は既定では無効で、設定で明示的に有効化した場合だけ `Ctrl` の2回叩きで開きます。

## 主な変更

- Sakura Pad を追加しました。メモ一覧、検索、並べ替え、新規作成、コピー、削除を持つ2ペインのウィンドウで、クライアント幅 520 logical px を境に、左右分割と `≡` で切り替える単一ペインへ形が変わります。編集の停止から少ししてから自動保存し、保存状態を編集面の見出し行に表示します。（#91、#92）
- Pad のショートカットは既定で無効です。設定画面の「Sakura Pad ショートカット」、または `sakura_settings.exe config set pad-shortcut double-ctrl` で有効にします。ジェスチャーは同じ側・同じデバイスの `Ctrl` を 500 ms 以内に2回叩いた場合だけ成立し、他のキーが挟まると成立しません。（#91）
- Pad のメモは `%LOCALAPPDATA%\SakuraInput\pad\memo.bin` に、現在の Windows ユーザー向け DPAPI で暗号化して保存します。最大 200 件、タイトル 256 UTF-16 単位、本文 65,536 UTF-16 単位です。一時ファイルへ書き出してから公開し、検証済みバックアップを1世代保持します。読めない既存データは上書きせず、偽の「保存済み」を表示しません。（#91）
- Pad の内容は入力履歴、学習ストア、ユーザー辞書、AI 文章変換のいずれにも渡しません。engine 側の host policy を renderer 所有の Pad に対して閉じ、履歴・学習・AI の各入口で明示的に拒否します。Pad の中でも通常のローカル変換と予測は使えます。（#91）
- ジェスチャー検出に使う Raw Input は、キーの文字やスキャンコードを保持せず、判定に必要な最小限のイベントへ落としてから状態機械へ渡します。（#91）
- 候補ポップアップと Pad が同じ配色・寸法定義を使うよう、パレットと寸法を `theme` へ切り出しました。Pad 固有の色は追加していません。Windows のハイコントラストではシステムの色を優先します。（#92）
- 一覧と編集面のスクロールバーを、それぞれのペインの地色に合わせた Sakura 独自の細いレールに置き換えました。ホイール、ドラッグ、ページ送りに対応します。（#92）
- IPC の protocol version を 20 へ上げました。Pad ショートカットの UI 状態が wire data に加わるため、v19 の Hello は拒否します。（#91）

## 含まれないもの

- **GitHub 同期は未実装です。** Pad 下部バーの同期ボタンは「GitHub 未設定」と表示するだけで、通信は行いません。メモは端末内にとどまります。
- Pad の dark テーマと Windows ハイコントラストは、自動検証だけで実画面確認が済んでいません。

## 検証

- workspace 全体の format、Clippy（`-D warnings`）、`cargo test --workspace` に成功しました。
- 実 renderer プロセスを使う Pad の UI テストで、2つの形の切り替え、コントロール配置、スクロールレールの幅と位置、ホイール、候補の非活性、DPI 変更を確認しました。
- Pad ストレージは v1 文書からの移行、境界値、`ReplaceFileW` の障害注入を含めて回帰しています。
- engine の host policy テストで、Pad からの履歴・学習・AI 各要求が拒否され、通常のローカル変換と予測だけが通ることを確認しました。

## 対応環境

Windows 11 build 22000 以降、x64、AVX 対応 CPU。32 bit ホスト用 DLL と ARM64 ネイティブ版は含みません。

## インストール前の確認

この Release は owner 承認済みの Authenticode 未署名版です。GitHub Release の `sakura_setup.exe` を取得し、同じ Release にある `release-manifest.txt` の SHA-256 と PowerShell の `(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash` が一致することを確認してから手動で実行してください。

未署名 installer は自動更新の Authenticode 検証を通らないため、自動取得・実行されません。updater の fail-closed 動作は維持されます。

アップグレードは versioned payload の side-by-side 切替で行われます。新しい payload の検証に失敗した場合は、TSF 登録を新しい版へ切り替えずに中止します。
