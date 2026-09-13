## テスト出力規約（Issue #111、owner判断、2026-08-30）

- 通常の`cargo test`は、ローカル、Codex作業、CI、releaseのすべてで`./ci/run-test-quiet.ps1`を必ず経由する。成功時は`PASS: <テスト名>`の1行だけを返し、通常ログをコンテキストへ流さない。失敗時は保存していたstdout／stderrを全量再表示し、元の終了コードを失敗理由へ残す。
- workspace全体の標準コマンドは`./ci/run-test-quiet.ps1 -Name 'workspace tests' -Command { cargo test --workspace --features 'sakura-engine/dev-fixtures,sakura-ime-eval/engine-fixture' }`。この2つのtest-only featureは、shipping buildから分離したfixture targetをCargoが省略しないために必要である。対象テストも同じ形で`-Command`内の`cargo test`だけを絞り、対象がengine fixtureなら`--features dev-fixtures`、ime-evalの実engine fixtureなら`--features engine-fixture`を残す。ラッパーが失敗を報告した後に原因を調べる場合だけ、必要な単独テストを生の`cargo test -- --nocapture`で再実行してよい。
- ラッパー自体を変更するときは`./ci/test-run-test-quiet.ps1`を実行し、成功出力が1行だけであること、失敗時にstdout／stderr／終了コードが戻ること、workflowに生の`cargo test`がないことを確認する。`cargo fmt`、`cargo clippy`、`cargo audit`はこのラッパーの対象外とする。
