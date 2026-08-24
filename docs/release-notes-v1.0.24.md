# Sakura Input 1.0.24

Release CI、更新installer、Named Pipe IPCのセキュリティ境界を強化するリリースです。通常の日本語入力とITエンジニア向け変換品質は変更せず、信頼できない低整合性プロセスやAppContainer、配布工程での差し替えに対する防御を追加しました。

## 主な変更

- engineのNamed PipeをData、Renderer、Controlの3境界へ分離し、各endpointで許可するrequestをserver側の全列挙allowlistに固定しました。低整合性・AppContainer・識別不能なclientからはAI文章変換を拒否し、管理操作とrenderer操作は低整合性Data endpointから実行できません。（#88）
- 最初のHelloに750 msのdeadline、processごと・endpointごとの接続上限、Dataとは独立したRenderer／Control capacityを追加しました。接続だけを保持するclientが管理経路や候補rendererを枯渇させない構成です。（#88）
- production clientはprotocol dataを送る前に、接続したpipe handleからkernelが返すserver PID、process tokenの整合性、Program Files配下の正規`versions/<release>/sakura_engine.exe`を検証します。Low-IL rival pipe instanceへ接続してもHelloや入力内容を送りません。（#88）
- updaterはdownload済みinstallerの同一file objectを、size、SHA-256、file identity、Authenticode、`ShellExecuteExW`による起動までwrite／delete共有なしで保持します。検証後のrename、置換、削除を防ぎ、確認したものと実行するものを一致させました。（#87）
- Release workflowをsecretless build jobと保護されたsigning jobへ分離し、GitHub Actionsをreview済みcommit SHAへ固定しました。build artifactのdigestとファイル単位provenanceをsigning jobで再検証し、署名materialを削除・不存在確認してからだけ成果物をuploadします。（#86）
- Sakura Input固有のpublisher certificate／SPKI pinは、owner管理のtrust anchorとrotation policyを確定してから導入する後続作業として分離しました。（#90）

## 検証

- IPC unit／integration test 26件、engineのendpoint認可・接続制御test 25件、updater test 11件に成功しました。
- 実AppContainer tokenを使うprivate-pipe testで、sandbox判定、engine process認証、Hello、通常キー入力まで成功しました。
- Low-IL non-AppContainerが既存ACL下でrival instanceを作れることを動的に再現し、production clientがserver identity不一致をHello前に拒否する回帰testを固定しました。
- `ReplaceFileW`、write、deleteがinstaller guard保持中に拒否され、guard解放後にだけ置換できることを確認しました。
- workspace全体のformat、Clippy、test、Release workflow policy、diff check、残存cargo／rustc／test process 0を確認しました。独立したLuna Maxレビューでも全security rubricがPASSしました。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このReleaseはowner承認済みのAuthenticode未署名版です。GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。

未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。今回追加したexact-file検証を含め、updaterのfail-closed動作は維持されます。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
