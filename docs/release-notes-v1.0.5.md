# Sakura Input 1.0.5

1.0.4で研究用経路だけを残していた自作のSakura-Rerank-Tiny-v1を、通常installerへ同梱するリリースです。

## 変更

- Sakura-Rerank-Tiny-v1のFP32 ONNXモデル、Rust worker、ONNX Runtime 1.28.0を通常installerとrelease bundleへ追加しました（#32）。
- 新規installationでは既定の`LongTextOnly` scopeが有効になり、長い読みの通常変換にある最大6件の辞書N-best候補をローカルで再順位付けします。既存ユーザーが明示的に`off`へ設定している場合は上書きしません。
- モデルはSakura Input作者の自作物としてMITライセンスで配布します。ONNX RuntimeのMIT Licenseとthird-party noticesもinstallerへ同梱します。
- release stagingとinstallerは、モデル、研究manifest、runtime、worker、ライセンス文書のサイズとSHA-256を検証します。installation後も登録を切り替える前に全neural payloadを再検証します。
- worker不在、モデルやmanifestの不一致、起動・IPC・推論失敗、timeout、古い結果では従来のローカル順位を維持します。Password、URL、Email、Digits、未知scope、`test_only`入力をworkerへ渡さず、クラウド送信も行いません。

## 品質状態

既存の研究評価ではGate A未通過で、final holdoutも未使用です。今回の同梱は、実用性に関するowner判断と自作物の配布承認に基づくものであり、Gate A/B合格を意味しません。モデルのWindows CPU cold/warm latency、10,000回以上のbatch-one測定、private working setは今後も独立した評価項目です。

同梱モデルは7,466,707 bytes、SHA-256 `b3fe1e0aa7229edfd0760162d648f10328b0d75224a9cd49f2ba986b7db2ccbd`です。

## 同梱辞書

- `system.dic` 39,349,040 bytes
- SHA-256 `b7d08643395181f6d214866f9bb98646de366dc71caa15320effe774bc4c1d90`

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このリリースの`sakura_setup.exe`はローカルビルドで、Authenticode署名を含みません。署名の検証はできないため、GitHubのReleaseページから取得し、公開されているSHA-256と一致することを確認してください。

アップグレードはversioned payloadのside-by-side切替で行われます。neural payloadの検証に失敗した場合は、新しいTSF登録へ切り替えずinstallationを中止します。
