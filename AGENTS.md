# AGENTS.md

このリポジトリでの作業指針は、まず `CLAUDE.md`、`README.md`、必要に応じて `DESIGN.md` / `PLAN.md` を参照してください。ここでは Cursor Cloud（Linux VM）環境に固有の注意点だけをまとめます。

## Cursor Cloud specific instructions

### 製品と実行環境の前提
- Sakura Input は Windows 11 x64 向けの日本語 IME（TSF ベースの in-process COM DLL ＋ out-of-process エンジン）です。完成品の GUI アプリ（`sakura-tsf` DLL、`sakura-engine`、`sakura-renderer`、`sakura-settings` など）は **Windows 専用**で、`windows-rs` / TSF / COM に依存し `#![cfg(windows)]` で囲まれています。**この Linux Cloud VM ではビルドも実行もできません。** 本番の CI は `windows-latest` 上で実行されます（`.github/workflows/ci.yml`）。
- したがって Linux VM で扱えるのは、以下の **クロスプラットフォーム crate だけ**です: `sakura-proto`、`sakura-core`、`sakura-neural-proto`、`dictc`、`sakura-neural-worker`。

### 最重要の落とし穴: デフォルトターゲット
- `.cargo/config.toml` がデフォルトビルドターゲットを `x86_64-pc-windows-msvc` に固定しています。そのため **素の `cargo build` / `cargo test` / `cargo clippy` は Linux 上ではリンクに失敗します。**
- Linux でクロスプラットフォーム crate を扱うときは、必ず `--target x86_64-unknown-linux-gnu` を明示してください。例:
  - ビルド: `cargo build --target x86_64-unknown-linux-gnu -p sakura-core -p sakura-proto -p sakura-neural-proto -p dictc -p sakura-neural-worker`
  - テスト: `cargo test --target x86_64-unknown-linux-gnu -p sakura-core -p sakura-proto -p sakura-neural-proto -p dictc`
  - clippy: `cargo clippy --target x86_64-unknown-linux-gnu -p <crate> --all-targets -- -D warnings`
- `cargo fmt --all -- --check` はターゲット指定なしでそのまま使えます。

### 既知の Linux 非互換テスト
- `sakura-neural-worker` の `sakura_runtime::tests::sibling_path_and_model_free_self_test_pass` は Windows のパス（`C:\payload\onnxruntime.dll`）を前提にしており、**Linux では失敗します（想定内、回帰ではありません）。** Linux ではこの 1 件を除いて neural-worker のテストは通ります。他の 4 crate は全て通ります。

### `rtk` と PowerShell スクリプトについて
- `CLAUDE.md` や `docs/` にある `rtk <cmd>`（例: `rtk cargo ...`、`rtk git ...`）の `rtk` は Windows ホスト側の開発ラッパーで、**この Linux VM には存在しません。** 素の `cargo` / `git` / `gh` をそのまま使ってください。
- `scripts/*.ps1`（`build-installer.ps1`、`build-dictionary.ps1`、`verify-*.ps1` など）と `ci/*.ps1` は Windows 専用で、この VM では実行できません。

### Linux で実行できる「動くもの」
- `dictc` は辞書コンパイラで Linux 上で実際に動きます。TSV ソース（`# license:` 宣言 ＋ `reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation` ヘッダ）と接続コストファイルから、`SKRADIC` バイナリ辞書イメージを決定論的に生成します。
- CLI 経路（`dictc --system ... --connection ... --output ...`）は出荷用の **2672 クラス固定タキソノミー**を要求します（テストの `parse_connection(.., false)` はこの検査を外していますが、CLI は要求します）。接続行列の `classes` は `2672` を指定してください。

### ツールチェーン
- `rust-toolchain.toml` が `/workspace` 内で Rust `1.96.0` を固定します（`rustup show` の active toolchain が 1.96.0 になります）。`/workspace` の外ではシステム既定の toolchain になる点に注意。
