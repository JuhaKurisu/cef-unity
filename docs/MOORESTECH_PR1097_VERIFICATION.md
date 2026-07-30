# moorestech PR#1097 調査レポートの検証

- 検証日: 2026-07-30
- 検証対象: <https://github.com/moorestech/moorestech/pull/1097> (2026-07-30 マージ, merge commit `b4dc00c`, レビューなし)
  - `docs/webui/cef-unity-frame-rate-investigation-2026-07-29.md` (初回報告)
  - `docs/webui/cef-unity-frame-rate-followup-2026-07-29.md` (続報)
  - `docs/webui/cef-unity-64f9a5f-stats-instrumentation.patch` (計装パッチ)
- 照合リビジョン: moorestech の cefunity ピン `64f9a5f` / レポート調査時の cef-unity main `8fc504e` / 検証時 HEAD `e14cf1e`
- 検証方法: レポート中の全コード主張を上記 2 リビジョンの実コードに照合、CEF 上流ソースを直接取得、moorestech 側ファイルを `gh api` で取得、計装パッチを `64f9a5f` の worktree で `git apply --check`

## 総合判定

**コード構造に関する主張はほぼすべて正しい**。行番号レベルで照合した約 25 項目が一致した。

**続報のヘッドライン結論 2 件は成立しない**。1 件はレポート自身の観測データと矛盾し、1 件は計装カウンタの意味の読み違いである。初回報告の機構説明にも 1 件誤りがある (§3)。

実測値そのものは単一マシン・n=1 の観測で再現不能だが、内部整合性チェックは通る (§4)。

## 1. 検証済みの主張

| 主張 | 検証結果 |
|---|---|
| `64f9a5f`→main は 101 コミット | ✅ `git rev-list --count 64f9a5f..8fc504e` = 101 |
| `iosurface_pool.m` は 101 コミットを通じ完全同一 | ✅ `git diff --quiet` 一致。**現 HEAD `e14cf1e` でも同一** = ピン更新では解消しない |
| 1ms `CFRunLoopTimer` 無変更 | ✅ `event_loop/macos.rs:188`(64f9a5f) / `:214`(main) ともに `0.001` |
| `+3/+6ms` flush 閾値 `[3.0, 6.0]` 無変更・定数リネームのみ | ✅ `server.rs:843` (`FLUSH_THRESHOLDS_MS`) → `:863` (`FLUSH_THRESHOLDS_MILLISECONDS`) |
| C# busy-wait は構造分離のみで無変更 | ✅ `SpinWait(64)`・4.5/7/7.5ms・probe 60F が `CefUnity.Core/Scroll/CefZeroFramePacer.cs:26,32,38,48,57,64` に同値で存在 |
| 全 drain → CEF pump 1 回、無制限 mpsc | ✅ `server/src/main.rs:117` が `std::sync::mpsc::channel` (無制限)、`macos.rs:162-190` が `TryRecvError::Empty` まで drain、`:155-158` で `do_message_loop_work()` 1 回。main の追加は `IN_TICK` 再入ガードのみ |
| 7 機構中 a08f585 のみが修正 | ✅ main に `suppression_cooldown` + `SUPPRESSION_RETRY_FRAMES = 60` (`server.rs:906,917,1653-1663`)。他 6 機構は無変更 |
| `g_receive_port` リーク・disconnect 経路の不存在 | ✅ `client/src/metal_texture.m:44,59` で connect 毎に allocate、解放コードなし。main にも `disconnect` を含む関数は皆無。`docs/REFACTORING_REPORT.md:219` の CLI-10 として自己診断済み |
| Play 停止毎のゾンビ化の機構 | ✅ `client/src/lib.rs:286-288` が `Ok(_child) => { // Server runs independently; we don't track its PID }` そのもの。waitpid も PID 保持もない |
| shutdown は fire-and-forget + 500ms 固定 sleep・終了確認なし | ✅ `client/src/lib.rs:358-366`。doc コメントは "wait for server to exit" と実装に反する記述になっている |
| dirty rect 未使用の全画面 Metal blit + `waitUntilCompleted` | ✅ `server.rs` の `on_accelerated_paint` は `_dirty_rects` を捨て、`iosurface_pool.m:160-176` が `sourceSize:{w,h,1}` 全面 blit + 同期待ち |
| Mach send は 10ms timeout | ✅ `mach_iosurface.c:141-148` |
| client は溜まった message を drain し最新以外を破棄 | ✅ `metal_texture.m:114-147` |
| `external_message_pump=1` と 1000Hz fallback の併存 | ✅ `server.rs` の `settings.external_message_pump = 1` |
| CEF VERBOSE ログが `--logging` と無関係に常時出力 | ✅ `LOG_ENABLED` はラッパ自前ログのみを制御。`settings.log_file`/`log_severity = VERBOSE` は無条件 (`server.rs:946` @64f9a5f / `:990` @main) |
| CEF の External BeginFrame は one-in-flight | ✅ 上流 `libcef/browser/osr/render_widget_host_view_osr.cc:1178-1181` が `if (begin_frame_pending_) return;`、`OnFrameComplete` で解除 |
| 両リビジョンとも `cef = "145.5.0"` | ✅ `crates/server/Cargo.toml:12` |
| ディレクトリ構造変更 (`Interop/Plugins`→`Plugins`) | ✅ main 直ビルドの成果物差し替えは不可、という注意書きも妥当 |
| moorestech の固定リビジョン | ✅ `moorestech_client/Packages/packages-lock.json` の hash = `64f9a5f3019d660e89a2909a7e1ca9d342aca5b1` |
| `MainGameUI.prefab:453-459` の `_resolutionScale: 1` / `_zeroFrameWaitMs: 10` | ✅ 455 行 / 459 行に一致 (`_enableLog: 0` も 456 行) |
| クラフト進行が rAF 駆動・`MAX_FRAME_DELTA_SECONDS = 0.25` で約 4 倍悪化 | ✅ `useHoldCraft.ts` の `loop` 内で進行判定、`holdCraftLogic.ts` に cap。算術も正しい |
| `REFACTORING_REPORT.md` の引用 (旧 40 件中修正済み 0 件・6 件悪化 / Editor 5h+ で 20-30fps 劣化) | ✅ 原文 `:453` / `:17` と一致 |
| 計装パッチは `64f9a5f` に適用可能・挙動不変 | ✅ 4 ファイル全て `git apply --check` 通過。`let issued = self.issue_begin_frame(...)` の束縛は `&&` 先頭項が常に評価されるため等価。edition 2024 + let-chains 既用なのでコンパイル可能 |

## 2. 成立しない結論

### 2.1 続報 結論2「BeginFrame の過剰供給は実在する」— 取り下げが必要

rAF が 24% の秒で 62 回/秒超・最大 80 回/秒 を「+3/+6ms flush が余剰フレームを実駆動している実証」としているが、**同レポート §2.8 の表では良い秒の `bf_f3+bf_f6` = 0、paint = 60** である。

コード側の理由も明確で、連続アニメーション中は次の 2 つが flush を止める。

- `server.rs@64f9a5f:1600-1602`: `damage_streak >= DAMAGE_STREAK_SUPPRESS_FLUSH (3)` で flush 予約自体を行わない
- `server.rs@64f9a5f:1631-1640`: BF#1 以降 2 paint 到着で残り flush をキャンセル (コメントで `begin_frame_pending_` による BF drop を回避する意図と明記)

つまり 80 BeginFrame/秒 を生む機構が観測データ側に存在しない。より素直な説明は**プローブの秒窓スキュー**である。レポート自身が maxGap 最大 1136ms を計測しており、真 60fps でも実測 1.33 秒分を「1 秒」と数えれば 80 になる。加えて §2.8 の悪い秒は flush 5〜29 に対し rAF が 11〜47 へ**低下**しており、flush 数と rAF は逆相関している。

再主張するなら、STATS の `bf_unity`/`bf_f3`/`bf_f6` と rAF カウントを**同一時間軸で突き合わせる**必要がある (計装パッチには既に両カウンタがある)。

### 2.2 続報 結論1「busy-wait は毎フレーム突入している」— カウンタからは言えない

`_dpFreshCount` / `_dpFallbackCount` は待機抑止パス (`CefUnityBrowserSample.cs@64f9a5f:701-704`、ブロックしない) でも加算される。一方 `_dpBlockSumMs` は待機ループを通った時だけ加算され (`:752`)、`blockAvg` は `fresh+fallback` で割る (`:490-491`)。したがって `fresh=81 fallback=35 idle=0` は「116 フレームが busy-wait に入った」証拠にならない。

ただし**結論の向きは救われる**。総 spin 時間 = `blockAvg × (fresh+fallback)` ≒ 356ms/2s ≒ 実時間の 18% は割り方に依らず成立し、実際にブロックしたのが一部フレームなら 1 フレーム当たりの spin は報告値より**大きい**。文言が不正確なだけで、主張自体は保守的である。

### 2.3 初回 §5.1「クラフトバーには待機抑止の成功経路がない」— 機構の誤り

抑止条件は `_streakScore >= 3 || _consecutiveInputFrames >= 3` (`:701`) で、`_streakScore` は fresh paint で +1 / paint 落ちで −2 の**入力非依存**カウンタである (`:761, 770`)。rAF アニメーションでも paint が続けば抑止に入る。

効かない真因は「fallback が約 30% あるため −2 減衰でスコアが閾値付近を上下し、抑止が定着しない」こと。結論 (クラフトバーで busy-wait が効いてしまう) は変わらないが、機構の記述は書き直しが必要。

## 3. 検証不能な主張

ゾンビ +1/サイクルの PID 一致、ポート +11〜13/サイクル、`copy_wait` 121〜157ms / 300〜440ms、ticks 崩壊、shutdown 時 SIGSEGV の `.ips`、FD 継承の lsof 観測、`sample` の 68%/99% — いずれも当該マシンの一回性の観測で再現不能。機構としてはすべてコード上成立する (Rust の `Command::spawn` が CLOEXEC を付けるのは自前 FD のみ、という FD 継承の前提も正しい)。

内部整合性チェックは通った。

- 続報 §2.9 t+29 行の `ticks=640` + `pump_max=439ms` ≒ 1.08 秒窓は、STATS が「1 秒**以上**の窓」で emit する実装と整合する
- 健常秒の `copy_wait 28-35ms / 60 copies` ≒ 0.5ms/copy は `iosurface_pool.m` のコード内コメント「Cost ~0.5ms on Apple Silicon」と一致し、計装が正しく動いていた傍証になる

## 4. 修正方針への指摘

### 4.1 優先順位の根拠が弱い

続報は「全画面同期 Metal コピーの廃止」を最優先へ昇格させているが、レポート自身のデータで健常時のコピーは **0.5ms/frame (実時間の 3%)** である。dirty rect 化で GPU 帯域を 96% 削っても節約は約 30ms/秒 に留まり、1fps 症状は説明できない。

実際の起点は**サーバーメインスレッド上の無制限ブロッキング `waitUntilCompleted`** で、外部 GPU 競合下で数百 ms 伸びることである。3 つの下位案のうち **非同期化 (completion handler / 次 paint 時のフェンス確認) が本命**で、dirty rect 化は帯域最適化として別枠に置くべき。

### 4.2 dirty rect 部分コピーは drop-in ではない

`iosurface_pool.m` は `POOL_SIZE 5` のラウンドロビンで転送先を回す (`g_pool_idx = (g_pool_idx + 1) % POOL_SIZE`)。差分だけ blit すると転送先には 5 フレーム前の内容が残っているため、非 dirty 領域が壊れる。サーフェス毎の damage 累積か、プール構成の変更が前提になる。この制約はレポートに記載がない。

### 4.3 有効な指摘 (そのまま着手可)

- `g_receive_port` の解放 (CLI-10 の実装) — コード確定のリークで、main HEAD でも未修正
- Child ハンドル保持 + shutdown 時の wait/kill — `lib.rs:286-288` の設計欠陥は明白
- spawn 時の FD 継承遮断
- `waitUntilCompleted` の非同期化 (§4.1)

## 5. 細部

- 行番号が `64f9a5f` と main を無記載で混在させている (`macos.rs:207-215`・`lib.rs:236-241`・`:1523-1542` は main、`server.rs:840-869` は 64f9a5f)。`cef_debug.log` の `server.rs:940-941` は実際は 946 (64f9a5f) / 990 (main)。実体はすべて正しい位置にあるので追跡は可能だが、rev 明記が望ましい
- moorestech は public リポジトリで、レポートには調査者のローカル実パスと cef-unity 内部監査の自己批判がそのまま載っている。意図的でなければ公開範囲の確認を推奨
