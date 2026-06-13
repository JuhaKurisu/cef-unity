---
name: cef-external-begin-frame
description: CEF OSR external BeginFrame の 1F 遅延は double-pump (BeginFrame 二度撃ち) で 0F 化できることを実証済み (2026-06-13)。手法・計測方法・効かない手法の記録
metadata: 
  node_type: memory
  type: project
  originSessionId: 8d272a18-5d91-4b70-b273-6efdda2db861
---

# CEF OSR external BeginFrame: double-pump による 0F 達成 (実証済み)

## 結論 (2026-06-13 実装完了・実測)

**「構造的に 1F 遅延が不可避」という旧結論は覆った。CEF フォーク無しで真の 0F を達成し、production 実装済み。**

手法 = **double-pump + reflush ループ + accel_frame_id 同期**:
1. EarlyUpdate 末尾: 入力送信 → `PeekAccelFrameId()` で BF#1 直前の paint カウンタ `_afiAtBf1` を記録 → `SendExternalBeginFrame` (BF#1) で renderer に当該フレーム内容を生成させる
2. PostLateUpdate 末尾 (`PumpAndRecvAccelerated`):
   - **settle 待ち**: `accel_frame_id` が `_afiAtBf1` を超える (= #A=BF#1 の即時 stale draw が計上された) まで待つ。これで baseline に #A を含め、後で遅着の #A を fresh と誤認しないようにする。上限 `_doublePumpSettleMs` (4ms)。#A に damage が無ければ stale paint 自体が生成されないので未計上でも安全
   - **baseline = PeekAccelFrameId()**
   - **reflush ループ**: `_doublePumpFlushIntervalMs` (2ms) 間隔で flush BF を撃ち直しつつ、`accel_frame_id != baseline` (= flush 由来の fresh paint #B) になるまでスピン。上限 `_doublePumpRecvMs` (12ms)。renderer の submit が何 ms 後でも次の flush が最新を draw するので取り逃さない
   - AFI 増分検出 → `TryRecvIOSurfaceTexture` で drain 受信 → 同フレーム反映 (0F)

**核心の同期保証**: server は `on_accelerated_paint` で IOSurface の Mach 送信を**完了してから** `accel_frame_id` を +1 する。よってクライアントが AFI 増分を観測した時点で、対応する IOSurface は既に受信ポートに enqueue 済み → drain で確実に取得できる (取りこぼし無し)。

**早着 paint 問題の解決**: `mach_iosurface_recv_texture` はキューを全 drain して最新だけ返す。旧実験の「最初の surface でスピン停止」は #A (BF#1 の stale draw) を掴んで 0f/1f 混在を招いていた。AFI baseline (#A 込み) を超える増分だけを採用することで #B (fresh) を確実に掴む。

実測 (testufo = 1440p 60fps フル画面アニメ・空 Unity シーン = 最悪条件):
- **0F 率 98-100%** (120 サンプル窓で fresh 118-120)、残りは 1F graceful フォールバック
- PostLateUpdate ブロック avg ~8ms (空シーン)。**実ゲームでは EarlyUpdate→PostLateUpdate の game logic 処理が CEF renderer (別プロセス) の生成と並列に進むため settle 待ちが即抜け、ブロックは draw レイテンシ ~3ms 程度に下がる見込み**。空シーンは renderer 待ちが全部ブロックに乗る最悪ケース
- 静止ページ対策: `_inputSentThisFrame` (入力ディスパッチ時 true) または直近 `ActivityWindowFrames`(8) 以内に fresh paint があれば active 判定で spin。完全静止時は idle パスで flush 1 発 + 非ブロック recv のみ → ブロック 0

## production 実装の所在 (2026-06-13)

- **Rust**: `crates/ipc/src/lib.rs` `ShmReader::peek_accel_frame_id()` / `crates/client/src/lib.rs` `cef_unity_peek_accel_frame_id` FFI。server.rs は変更不要 (double-pump は完全にクライアント側)
- **C# FFI 宣言**: `NativeMethods.g.cs` (unityproject + csharp 両方) に `cef_unity_peek_accel_frame_id`、`CefUnity.cs` (両方) に `Browser.PeekAccelFrameId()`
- **C# ロジック**: `CefUnityBrowserSample.cs`。SerializeField: `_doublePump`(既定 true)/`_doublePumpSettleMs`(4)/`_doublePumpRecvMs`(12)/`_doublePumpFlushIntervalMs`(2)。accelerated paint 経路でのみ作動 (software は従来の単発取得)。検証ログ `[CefUnity] double-pump: fresh/fallback/idle/block_avg/block_max`

## メカニズム (なぜ 1F だったか / なぜ double-pump で消えるか)

- CEF の `SendExternalBeginFrame` は BeginFrameArgs を **deadline=null (TimeTicks())** で発行
- → renderer cc scheduler は main thread の commit を待たず、**前回 activate 済みの tree を即 submit**
- → display は BF 時点で aggregate できるもの (= 前フレーム内容) を即 draw → 1F
- 2 発目の BF を renderer submit (~3ms) 完了後に撃つと、display は submit 済み最新内容を draw する
- CEF の `begin_frame_pending_` ガード: ack (OnFrameComplete) 前の 2 発目は**無言で drop** されるが、ack は ~3ms で返るため 4ms 後の flush は drop されない (drop されても 1F フォールバックで安全)

## 効かない手法 (実測済み)

- **`--run-all-compositor-stages-before-draw` (Viz full-pipe)**: browser+GPU+renderer 全プロセスに付与しても**完全に無効** (delta=1f、latency 2.7ms 不変)。headless CDP beginFrame の 0F はこのスイッチによるものだが、CEF の external BF 経路では効かない
  - 注: renderer への伝播は content の自動リストに無く `on_before_child_process_launch` で明示付与が必要 (実装済み、`cef_unity_full_pipe` toggle)
- 旧記録: `--disable-gpu-vsync`, `--disable-frame-rate-limit`, `--enable-begin-frame-control` (paint 停止する), CFRunLoopTimer 短縮 — すべて効果なし

## Ground truth 計測法 (server.rs に実装済み、toggle: `<TMPDIR>/cef_unity_latency_test`)

`paint_unity_frame` マーカーは「paint 時点の最新 BF 番号」を書くだけでペアリング保証が無く**信頼できない** (旧ブロック版の delta=0 はこの欠陥による見かけ)。正確な計測は:
- BF 発行直前に `Frame::execute_java_script` で `document.documentElement.style.backgroundColor` に frame 番号をエンコード (R/G 各 8 段階 ±15 耐性、B=128 sentinel、URL は about:blank に強制)
- `on_accelerated_paint` で pool IOSurface を `IOSurfaceLock` → 画素 (0,0) をデコード → 真の delta histogram をログ

## 実験 toggle (すべて `std::env::temp_dir()` = /var/folders/.../T/ のマーカーファイル、デフォルト OFF)

調査時に使った temp ファイル toggle (`cef_unity_latency_test` 等) は production 実装に伴い**すべて撤去済み** (server.rs はクリーンに git checkout で戻した)。ground truth 画素計測 (背景色に frame 番号エンコード → IOSurfaceLock で読み出し) は再検証が必要なら server.rs に再実装する。full-pipe スイッチ (`--run-all-compositor-stages-before-draw`) は CEF external BF 経路では**無効**と実証済み (headless CDP の 0F はこれだが CEF では効かない)。

## 既知の特性 / 残課題

1. **空シーンのブロック ~8ms**: renderer 生成待ちが全部 main thread ブロックに乗る最悪ケース。実ゲームでは game logic と並列化されて ~3ms 程度になる見込み (要実機検証)。さらに削減するなら server 側タイマーで flush を撃つ方式
2. **reflush で renderer の rAF が増える**: 1 Unity フレームに BF を複数撃つ (settle 1 + reflush 1〜数発)。時間ベースのアニメは無害、rAF 回数依存コードは速くなる。steady state では renderer が速ければ flush 1〜2 発で収束
3. settle/recv/flushInterval 予算はページ複雑度依存。SerializeField で field 調整可。block_avg/max をログ出力するので実測で詰められる
4. 完全 0F を画素レベルで再保証したい場合は ground truth 計測を server.rs に再実装して検証する

## 参考

- [Chromium: Life of a frame](https://chromium.googlesource.com/chromium/src/+/lkgr/docs/life_of_a_frame.md)
- [CEF Issue #4166](https://github.com/chromiumembedded/cef/issues/4166) OnPaint visually incomplete frame
- [CEF Issue #4033](https://github.com/chromiumembedded/cef/issues/4033) macOS external BF + shared texture で paint 来ない報告 (本環境では再現せず)
- headless `HeadlessExperimental.beginFrame` + full-pipe (CEF では効かなかった)
- `external_begin_frame_source_mojo.cc` の ack = 全 frame sink DidFinishFrame (double-pump の理論的裏付け)
