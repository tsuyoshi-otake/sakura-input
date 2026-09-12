## タスクバー入力モードasset（Issue #26）

- `crates/sakura-tsf/assets/mode-indicator`には、全6モードの16px／32px・dark／light用premultiplied BGRA assetがある。第三者製品のassetを含めたり、その解析内容を公開文書へ記載したりしない。
- assetはSakura Input独自のYu Gothic UI Semibold字形で、`scripts/generate-mode-indicator-assets.ps1`から再生成する。実行時フォント描画へ戻さない。
- `crates/sakura-tsf/src/mode_item.rs`はDPI境界でassetを選び、top-down 32-bit DIBからHICONを生成する。中間bitmapは必ず削除し、返却HICONの所有権はTSFへ渡す。
- 直接入力は斜線付き`A`、半角英数は通常の`A`であり、同じassetへ統合しない。全モード・サイズ・テーマの透過、alpha、premultiplication、一意性、HICON生成を総当たりテストする。

