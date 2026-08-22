# Sakura Input 1.0.19

確定した語の末尾と次の入力が文法的に続く場合、そのつながりを次の通常変換へ安全に反映するリリースです。

## 主な変更

- `考慮漏れ`を確定した直後に`ないか`を変換した場合、前回確定末尾と今回の読みを限定的に再解析し、`ないか`、`無いか`を`内科`より上位へ出せるようにしました。（#83）
- この処理は`ないか`専用ではありません。前回確定候補のexact dictionary edge、確定surface、typed context ID、辞書の接続・単語コストを使う汎用のbounded cross-commit bridgeです。新しい候補は作らず、今回の通常変換ですでに到達可能な候補だけを再評価します。
- `left_id`と`right_id`を型で分離し、辞書node側のIDを過去terminal stateとして誤用できないようにしました。過去の解析をzero-costで復活させたり、固定bonusやmagic IDで順位を変えたりしません。
- exactユーザー辞書と明示的な選択学習を優先します。association無効、前回確定なし、医療文脈、別session、句読点・mode・focus・document context・caret・host edit・undo・reconversion後では、従来のcurrent-only変換へ戻します。
- TSFが前回commitのexact rangeとcollapsed caretを次の実キー入力時に再検証します。文書上の隣接を証明できない場合は、protocol v19の`ResetDocumentContext`でcarry、bridge、commit recency、undo provenance、prediction cacheをclearします。resetが失敗した場合はengine linkを破棄してfail closedにします。
- bridgeは同じengine sessionのmemoryだけに保持します。Password、URL、Email、Digits、unknown/unclassified、test-onlyでは保存も参照もせず、入力履歴、learning store、log、disk、neural workerへtail textを渡しません。

## 検証

- `考慮漏れ`、`情報漏れ`、`情報が漏れ`と、別語彙の`検討 + しますか`で汎用経路を確認しました。
- `診療科は`、`受診先は総合病院の`ではcurrent-onlyの候補一覧全体が変わらないことを確認しました。
- common pathの追加heap allocationは0です。release buildの5,000回測定ではtarget変換全体のp99が0.238 ms、48-byte tailと96-byte currentの上限ケースでもp99が1.053 msで、既存の20 ms変換予算以内でした。
- release workflowはversion整合性、format、strict clippy、workspace test、残存test process、辞書再現ビルド、release binary、installer warnings、installer manifestのサイズとSHA-256を検査します。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

署名secretが完全に設定されている場合、release workflowは全配布binaryとinstallerをAuthenticode署名して検証します。署名secretがない場合はowner承認済みの未署名releaseとして公開し、部分設定の場合はreleaseを失敗させます。

未署名で公開された場合は、GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
