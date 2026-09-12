## Issue #27 Sakura候補ポップアップ（自動検証・通常light実画面確認済み）

- 候補表示はrenderer所有のWin32ポップアップであり、non-activating、click-through、キャレット追従、DPI対応、UI Automation公開を維持する。engine／TSFが持つ候補順、選択、ページ、候補種別の意味をrendererが変更してはいけない。
- Sakura独自の見た目は、low-contrastの暖色系neutral、Yu Gothic UI、28 logical px行、候補番号／文字列／注記の列、260–480 logical pxのcontent-aware幅、控えめな種別・ページfooter、passiveなページ位置rail、選択行のmuted sakura 2 logical px railである。候補文字列を主階層とし、注記・ページ情報は補助階層にする。
- light／dark paletteに加え、Windows high-contrastではsystem roleを使う。UI-less hostには`ITfUIElement`候補データ経路を保ち、popupの可視性に依存させない。compact／expanded表示は既存のengine semanticsとキー操作を維持し、新しい候補定義や操作を追加しない。
- 実装は通常のWin32 popupをGDI（`CreateFontW`／`DrawTextW`／brush）で描く。layered windowやDirectWriteを使う設計として説明しない。これはrendererの描画境界だけの変更で、候補用raster assetの再生成は不要であり、Issue #26のmode-indicator assetを変更・流用しない。
- unit testと実renderer processのintegration testにより、compact／expanded semantics、260–480 logical px幅、DPI変更、non-activation、キャレット追従、ページ、数字選択、UI Automation公開は自動検証済み。最新版再インストール後の通常light実画面スクリーンショットでは、候補本文、右側annotation列、淡い選択面と桜色rail、予測footer、`1–9/9`ページ表示を目視確認済み。Issue #27の検証記録ではmode-indicator assetに差分がないことも確認済み。残る受け入れ作業はdark／Windows high-contrastの実画面確認だけである。
