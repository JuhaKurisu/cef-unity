---
name: cef-external-begin-frame
description: CEF OSR external BeginFrame の設計記録。0F 化は server-side flush (2026-06-15)。スクロール低下は damage-streak 検出による flush 動的抑止で解決 (2026-07-02)。rAF 138Hz バースト・testufo pause 誤検知も同時解消。recv フック位置 (present より後) の課題は未解決。
metadata: 
  node_type: memory
  type: project
  originSessionId: 8d272a18-5d91-4b70-b273-6efdda2db861
---

# CEF OSR external BeginFrame: 0F 化 (server-side flush が最新の最適解)

## 1F 遅延 再調査 (2026-07-02): recv フックは present より後 = 実画面は delta+1

- 実測 (SampleScene): delta ほぼ全サンプル 1F (avg=1.00-1.02, buckets[1:] 支配的)、recv_ok は 86-115/120 と高い。「取得失敗」ではなく「常に 1 フレーム前の BF に対応する paint を受信」する定常 1F。原因は既知の通り軽シーンで PostLateUpdate recv (~+2-3ms) < paint 到着 (~+5-9ms、renderer submit 1.5-3ms + flush +3ms + draw/blit/Mach が物理下限)
- **新発見: recv フックは PostLateUpdate サブシステムリスト末尾に Append されており、Unity 6000.3 の PostLateUpdate 内には `FinishFrameRendering` (描画発行) と `PresentAfterDraw` (present) がある → 現在の recv/UpdateExternalTexture は present の後に実行され、次フレームの描画にしか乗らない。実画面遅延は計測 delta より 1F 悪い (delta=1 → 実質 2F)。過去の「0F 96%」も受信時点の計測で画面反映は未検証だった**
- **「recv を WaitForTargetFPS の後に挿す」案は棄却**: Unity 6000.3 に WaitForTargetFPS サブシステムは無く、limiter sleep は次フレーム先頭の `TimeUpdate.WaitForLastPresentationAndUpdateTime`。その後に recv しても反映は次フレームの present → 真の 0F にならない
- 提案 (未実装): Step1 = recv フックを PostLateUpdate 内の描画前 (PlayerUpdateCanvases より前) に移動 → ノンブロッキングのまま実画面 1F 短縮。Step2 = 同位置で予算適応 busy-wait (フレーム経過+待ち上限 < ~12ms の時だけ、PeekAccelFrameId 増分まで上限 ~5ms) → 真の 0F。過去の fps 転落 3 因 (client reflush IPC フラッディング / 12ms+ spin / micro-sleep オーバースリープ) は server-side flush + 予算ガードで全て排除される。間に合わなければ即 1F フォールバックで fps 影響ゼロ

## スクロール低下 解決済み (2026-07-02): damage-streak 検出で flush を動的抑止

「スクロール時にかなり遅くなる」の根本原因と修正 (server.rs 実装・deploy 済み、実測検証済み):
- **原因**: スクロール中は全 BF に damage が乗るため、flush 込み 3 BF/フレーム = draw+blit+Mach送信が 3 倍 → renderer/GPU 飽和 → `begin_frame_pending_` ガードが後続 BF を silent drop → コンテンツ欠落。実測 (Wikipedia スクロール): flush ON で recv_ok 95-110/120 (実効 ~52fps)・content std 5-8ms、flush OFF なら 119-121/121 (完全 60fps)・std 0.75ms
- **flush#2 の paint-count スキップだけでは不十分** (recv_ok 86-115 で改善僅か)。2 BF/フレームでも飽和する
- **解決 = damage streak 抑止**: `send_external_begin_frame` で「前フレームに paint があったか」を PAINT_COUNT 差分で判定し、`DAMAGE_STREAK_SUPPRESS_FLUSH`(3) フレーム連続で paint があれば連続描画中とみなして flush を撃たない (BF#1 のみの綺麗な 60Hz 駆動)。孤立入力は streak が切れるので従来通り flush で 0F
- **検証結果**: ①Wikipedia スクロール recv_ok 117-121/121 (flush OFF 同等) ②testufo が 60fps@60Hz を報告 (旧: 138fps バースト + 再同期永続失敗) ③5Hz 離散更新 + 8ms 負荷で delta buckets [0:9,1:1] = 0F 90% 維持 ④**重要な副次発見: 旧構成の testufo/mouserate「10fps」はページが bursty rAF を検知して pause していたため。rAF 正常化後は同ページが 60fps で連続アニメする** — 「低fps感」の正体はこれ
- トレードオフ: 連続アニメ/スクロール/キーリピート中のコンテンツは 1F (連続モーションでは知覚されない)。離散入力は 0F 維持。30fps 動画等 (1 フレームおき damage) は streak が伸びず flush 継続
- 付随変更: FLUSH_THRESHOLDS_MS [3,5]→[3,6] (flush#1 paint の到着観測を跨ぐため)、flush#2 は BF#1 以降 2 paint 到着済みならスキップ
- テスト補助 (C# サンプル): `<TMPDIR>/cef_scroll_test` マーカーで毎フレーム ±60px ホイール注入 (3秒毎に反転)。`cef_load_url` トリガーは Time.frameCount>60 に遅延 (初期ナビゲーションとの競合で LoadUrl が負ける race があった)。uloop execute-dynamic-code は isolation level 0 で無効のため、PlayMode への介入は temp ファイルトグル方式を使うこと
- 計測手順 (再現用): Play 開始 → `cef_load_url` に対象 URL → 12s 待ち (スクリーンショットでロード確認) → `cef_scroll_test` 作成 → clear-console → 13s 待ち → `verify:|jitter` ログ取得。判定は recv_ok/120 (実効fps) と content std (ジッタ)。server 変更は deploy.sh のみで反映可 (server は Play ごとに再起動、Editor 再起動は dylib 変更時のみ)

## 実効 fps 調査 (2026-07-02): パイプラインは 60fps 健全、副作用は rAF 138Hz バースト

「CEF が 60fps より低く見える」調査の実測結果 (Unity Editor + SampleScene、組み込み計装 verify/jitter で計測):
- **パイプラインは 60fps を完全供給**: rAF アニメーション (data: URI) で content 間隔 mean=16.67ms std=0.5ms、recv_ok=120/120、recv_fail=0。8ms 擬似ゲーム負荷 (`cef_fake_work`) でも維持
- **低 fps に見える第一原因はページ側の再描画頻度**: testufo.com/mouserate はページ自体が ~10Hz でしか repaint しない → content mean=100.0ms (きっかり 10fps)。external BF は「描く機会」を与えるだけで、damage が無ければ paint は来ない
- **副作用 (実バグ): renderer の rAF が ~138Hz で不規則バースト発火**: server-side flush により 1 Unity フレームに BF 最大3発 (0/+3/+5ms) → rAF が 2.3回/フレーム。testufo 本体は「138 fps」を報告し「Browser pause detected, resynchronizing」が永続 (同期不能)。影響: (a) rAF 回数ベースのアニメは ~2.3 倍速、(b) rAF タイムスタンプ間隔が不均一 (3/2/11.7ms) → タイムスタンプベースのモーションのサンプリングが不均一 = judder として「低 fps 感」を生み得る、(c) fps 計測系ページは誤動作
- **`<TMPDIR>/cef_no_server_flush` トグルで flush 無効化すると rAF は正常 59-60Hz** (testufo 実測 59fps@60Hz)。content 供給は 60fps のまま、trade-off は 1F 遅延復活
- 改善案 (未実装): flush 間隔を 0/+3/+5 でなく均等割り (uniform 180Hz) にすれば rAF タイムスタンプが均一化し judder は解消方向 (回数ベース 3倍速は残る)。または adaptive flush (fresh 取得済みなら flush2 スキップ) でバースト削減

## 最重要 (2026-06-15): クライアント busy-spin はスクロールを重くする → server-side flush で解決

**クライアント側 double-pump (Unity main thread で flush + spin 受信) は 0F を達成するが、スクロール等のアクティブ時に「重さ」を生む。** 原因 (実測で特定):
- spin は `while(){}` の busy-wait で main thread を占有し、待っている相手の CEF renderer/GPU プロセスを**兵糧攻め**にする → fresh paint が間に合わず fallback 連発。実測: 負荷時 fresh 13/fallback 100 = **コンテンツ実効 7fps** (Unity は 60fps 表示なので fps は落ちないのに中身がカクつく)
- reflush ループ (2ms 毎に flush) が **IPC コマンドチャネルを溢れさせ**、`SendExternalBeginFrame` 送信自体がブロック (サーバーが Metal blit `waitUntilCompleted` で固まり drain できない) → ブロックが 46ms に膨張、Unity 20fps に転落
- **micro-sleep/yield は無効**: `Thread.Sleep(1)` は macOS で 10ms+ オーバースリープ、`Thread.Yield()` も負荷時にデスケジュールされ、どちらもフレーム 50ms に悪化。「待ち方」を変えても「フレーム内で GPU バウンドの相手を待つ」結合は切れない
- 本質: **double-pump を Unity フレーム内でブロック待ちすると、Unity の fps が CEF のコンテンツ生成時間に結合する**。CEF が重い (重ページ/スクロール repaint/GPU 競合) と Unity 全体がカクつく
- 計測の決定打: フレーム時間 std (機構1=present ジッタ) と content 更新間隔 std (機構2=サンプリングジッタ) を分離。ON: frame std 1.6ms→負荷時 50ms、OFF: frame std 0.44ms 安定

### 解決策: server-side flush (実装・実証済み 2026-06-15)

double-pump をクライアントからサーバーへ移し、**Unity main thread を一切ブロックしない**:
- クライアント: EarlyUpdate で input + `SendExternalBeginFrame(N)` を**1 回だけ**送る。PostLateUpdate は `TryUpdateTextureOnce()` の**ノンブロッキング受信のみ** (spin/reflush 撤去)
- サーバー (`server.rs`): `send_external_begin_frame` で BF#1 を撃ち `pending_flush` を予約。event loop の **1ms tick** (`process_pending_flushes`) が +3ms/+5ms (`FLUSH_THRESHOLDS_MS`) で内部 flush を発行。flush は CEF host へのローカル呼び出しなので **IPC フラッディング無し**。fresh #B は従来通り Mach 送信 + accel_frame_id++
- 待ち (renderer submit) は**サーバースレッド上**で Unity フレームと並列に進む → 結合が切れる

実測 (server-side flush):
- **フレームペーシング: std 0.41-1.1ms (OFF 同等の滑らかさ)、46ms ブロック消滅** → 重さ解消
- **コンテンツ: 毎フレーム更新 (60fps)** (旧 spin の 7fps から回復)
- **0F 達成条件**: PostLateUpdate (recv) が サーバー flush 結果 (~EarlyUpdate+6ms) より後に来る必要がある。
  - 実ゲーム (EarlyUpdate→PostLateUpdate 間に 8ms+ の処理) → **recv が +8ms 以降 → 0F**。擬似 8ms 負荷で **end-to-end delta=0 が 96% (buckets [0:109,1:4])** を実証
  - 空シーン (処理ほぼ 0、PostLateUpdate が +2ms) → recv が flush より早く 1F (ただし滑らか)。**limiter sleep は PostLateUpdate より後なので work が無いと recv は前倒しになる**
- → ユーザーの実ゲームのスクロール時は 0F + 滑らかになる見込み (実ゲームでの最終確認は要)

### 未了 / 次の一手

- 旧クライアント double-pump コード (`PumpAndRecvAccelerated` + `_doublePump*` フィールド) は**現在 dead code** (未使用・警告なし)。要クリーンアップ
- 空シーンでも 0F を強制したいなら、recv hook を `WaitForTargetFPS` (limiter sleep) の**後**に挿し、present 直前で受信する案 (要 PlayerLoop 調査)
- 機構2 (scroll 曲線の frame_time サンプリングジッタ) は server flush で発行時刻が tick に揃うため改善方向だが未計測
- 計測計装 (jitter ログ: frame/content std) は `_enableLog` 配下に残置

---

## 旧記録: client-side double-pump による 0F 達成 (2026-06-13、重さ問題が判明し server-side へ移行)

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
