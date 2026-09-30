# Sakura Input 2.0.9

- AI文章変換と選択文字列の校正で使うモデルを`gpt-5.6-luna`から`gpt-6-luna`へ変更しました（#271）。Sakura Inputメニューと設定画面の表示も「GPT-6 Luna」になります。ChatGPT Subscription（Codex CLI）経由でも同じモデルを使います。
- Effort（low〜max）とTier（priority）の選択肢は2.0.8と同じです。送信先のEndpointまたはアカウントで`gpt-6-luna`を利用できない場合は、別モデルへ切り替えずにエラーで終わります。保存済みAPIキーが`gpt-6-luna`を使えるかは、`scripts/test-ai-api-key.ps1`で確認できます。

このリリースの検証は、送信するモデル名、応答モデルの一致確認、履歴記録の単体テストとfake Responses APIによる契約テストです。実際のResponses APIとCodex CLIを使った`gpt-6-luna`での変換は、リリース時点では検証していません。

このインストーラーはAuthenticode未署名です。手動導入する場合は、GitHub Releaseに掲載されたSHA-256とファイルのハッシュを照合してください。自動更新では署名付き更新manifestによるサイズ・SHA-256・未署名状態の検証を行います。
