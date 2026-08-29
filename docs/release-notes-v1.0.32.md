# Sakura Input 1.0.32

system dictionaryのfile-backed footprintを縮小し、全角英数変換の不要な走査と、neural workerに残っていた未使用の独自SIMD経路を整理するリリースです（#109、#110）。候補の表記、順序、cost、辞書detail identity、ONNX model/tensor contractは変更していません。

## 主な変更

### system dictionary format v2（#109）

辞書を起動時にowned objectへ展開しない既存のmmap設計を維持したまま、固定recordとindexの幅を縮小しました。trie nodeへlabelを統合し、entryを24 bytesから16 bytesへ縮小し、surface offsetは16件ごとのrestart offsetだけを保持します。annotationは全entryの固定slotではなく、entry ordinalで参照するsparse indexへ移しました。

同じpinned sourceから生成した辞書は47,561,532 bytesから37,381,940 bytesへ縮小し、10,179,592 bytes（21.403%）削減しました。624,205 entries、822,995 trie nodes、505,060 surfacesを全件比較し、storage version以外の意味差分は0件です。

first-touch測定ではpage faultsが12.464%、working setが12.461%減りました。これはfile-backed dictionary footprintの結果であり、engine private heapが同率で減るという意味ではありません。warm conversionの中央値はp50 +0.529%、p95 +0.206%、p99 +4.487%で、固定した5%回帰上限内です。

readerはformat v1とv2の両方を受理します。未知version、欠落・重複・重複範囲table、範囲外cost、非zero reserved、壊れたUTF-8、annotation ordinal不整合は候補へ部分適用せず、辞書open時にfail closedで拒否します。

### 全角英数変換の走査削減（#110）

解決済みの英字・数字・記号policyがすべて全角の場合、先頭が変換対象なら不要なrun scanを省略します。先頭がspaceやcontrol文字の場合は既存scannerを使うhybrid方式とし、pass-through主体の入力を悪化させません。

同一hostのrelease cycle比較では、all-full ASCIIが5.45%、identifierが9.10%、FollowMode + FullAlnumが10.04%、space/control-heavyが5.86%短縮しました。通常の1-key、ASCII、日本語、mixed経路は変更前比±1.3%以内です。O(N)、zero-allocation、unaligned input対応、UTF-8境界、overflow prefixの動作を維持します。

### SIMD ownershipとCI監査（#110、#70）

neural workerの実score pathから呼ばれていなかった独自SIMD summaryとCPU tier reportingを削除しました。候補scoreは従来どおりONNX Runtimeが所有します。protocol v1のresponse長・offset・versionは維持し、新workerはlegacy tier slotへ0を書き、engineは旧workerのnonzero tier responseも受理します。

coreのCPU feature判定は従来どおり`OnceLock`で起動時に1回だけ行い、hot pathは選択済みfunction pointerを使います。Windows CIでは、fresh targetへ生成した実assemblyについてscalar、AVX+SSSE3、AVX2、AVX-512 shared kernelのsymbolと必要命令を検査し、hot pathのCPUID、禁止register、symbol欠落・重複を拒否します。mutation self-testはCRLF/LFの両checkoutでno-op mutantを成功扱いしません。

## 互換性と安全性

- 候補の表記、順序、cost、reading、品詞・接続ID、辞書detailのexact entry ordinalは変更していません。
- v2 writerは表現できない値をtruncateせず、source位置付きerrorでbuildを停止します。
- ONNX model、tensor、ranking、fallback、privacy boundaryは変更していません。
- CPU feature検出を入力ごとに実行しません。
- Password、URL、Email、Digits、未知・未分類scope、test-only入力の既存fail-closed境界は変更していません。

## 検証

- workspace全体のformat、Clippy（`-D warnings`）、`cargo test --workspace`に成功しました。
- v1/v2辞書の全entry・trie・surfaceを比較し、canonical semantic digestが一致しました。
- truncated、overflow、duplicate、overlap、reserved、UTF-8、ordinalのnegative testに成功しました。
- 16/31/32/63/64/65-byte境界、offset付きunaligned slice、NUL/control、mixed UTF-8、overflow prefixを検証しました。
- assembly mutation self-testは5/5を拒否し、fresh-target実assembly gateに成功しました。
- 実行後のrepository由来cargo、rustc、test runnerの残存が0件であることを確認しました。

## 対応環境

Windows 11 build 22000以降、x64、AVX対応CPU。32 bitホスト用DLLとARM64ネイティブ版は含みません。

## インストール前の確認

このReleaseはowner承認済みのAuthenticode未署名版です。GitHub Releaseの`sakura_setup.exe`を取得し、同じReleaseにある`release-manifest.txt`のSHA-256とPowerShellの`(Get-FileHash .\sakura_setup.exe -Algorithm SHA256).Hash`が一致することを確認してから手動で実行してください。

未署名installerは自動更新のAuthenticode検証を通らないため、自動取得・実行されません。updaterのfail-closed動作は維持されます。

アップグレードはversioned payloadのside-by-side切替で行われます。新しいpayloadの検証に失敗した場合は、TSF登録を新しい版へ切り替えずに中止します。
