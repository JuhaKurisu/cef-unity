# CEF-Unity 設計リファクタリングレポート

- **作成日**: 2026-07-13
- **対象コミット**: eb7a098 (main)
- **目的**: 設計レベルのリファクタリング候補の全件記録。将来のセッション (Opus 等) がこのレポートだけを読んで修正作業に着手できることを意図している
- **分析範囲**: Rust server / Rust client / IPC crate / Unity C# 層の全ソース (約 10K 行)

---

## このレポートの使い方 (修正作業者向け)

1. まず「§1 絶対不変条件」を読むこと。**性能実測で確定した仕様**であり、どのリファクタリングでも命令列レベルで保存する必要がある
2. 各発見は ID (SRV-n / CLI-n / IPC-n / CS-n) で参照する。修正着手時はその発見の「リスク」欄を必ず確認する
3. 実施順は「§7 推奨ロードマップ」に従う。ワイヤプロトコル変更を伴うものは 1 コミットに束ねる
4. 修正完了後は「§8 回帰確認プロトコル」を実施する。**性能計測は必ず Unity Editor を再起動してから行うこと** (Editor 5h+ 稼働で CEF が 20-30fps に劣化する計測の罠がある)
5. 行番号は 2026-07-13 時点のもの。修正が進むとずれるため、シンボル名でも検索すること

---

## §1 絶対不変条件 (性能実測で確定した仕様 — 変更禁止)

以下は実測チューニングの成果物。リファクタリングは「構造だけ変え、これらの呼び出し順序・タイミング・値を 1 ビットも変えない」こと。

| 不変条件 | 場所 | 根拠 |
|---|---|---|
| server tick の順序: `drain_commands` → `process_pending_flushes` → `do_message_loop_work` | `crates/server/src/event_loop/macos.rs:119-133` | 0F 化実証シーケンス |
| CFRunLoopTimer 1ms interval | `macos.rs:185-193` | 同上 |
| `iosurface_pool_copy_and_get` の同期 blit (`waitUntilCompleted`) と `mach_iosurface_server_send` は `on_accelerated_paint` と同一スレッド・同一呼び出し内で完結 | `crates/server/src/iosurface_pool.m` / `server.rs:262-329` | クロスプロセス IOSurface は waitUntilCompleted 以外の同期不可 |
| shm メタデータは `unity_frame` を `frame_id` 増分より**先に**書く | `server.rs:323-325` (macOS/Windows 両ブロックに存在) | 順序不変条件 |
| POOL_SIZE=5 (server) / IOSURFACE_CACHE_SIZE=4 (client) | `iosurface_pool.m` / `metal_texture.m:15` | 実測 60fps 構成 |
| Mach send timeout 10ms | `mach_iosurface.c` | on_accelerated_paint をブロックしないための実測値 |
| client の drain-latest recv ループ | `metal_texture.m:117-147` | 60fps 実証の中核 |
| `peek_accel_frame_id` → flush → recv の double-pump 契約 | `crates/client/src/lib.rs` | 同上 |
| `wait_fence` → `open_or_cached` の呼び出し順序、`last_waited` の単調スキップ条件 | `lib.rs:1548-1551` / `d3d11.rs` / `d3d12.rs` | fence 同期の正しさ |
| `send_external_begin_frame` の発行タイミング (lock → send → return の命令列) | `lib.rs:1156` | 0F 化の中核 |
| `FLUSH_THRESHOLDS_MS` / `DAMAGE_STREAK_SUPPRESS_FLUSH` / `PAINT_COUNT` の読み取りタイミング | `server.rs:715-746, 1443-1550` | damage-streak flush 抑止 = スクロール 60fps の要。ずれると 52fps 再発 |
| C# 側 EarlyUpdate → PostLateUpdate のフレーム内順序、FreshPaintMinDelayMs=4.5 等の定数 | `CefUnityBrowserSample.cs:533-649, 663-742` | 0F 待ちロジック |
| macOS タイマーの意図的リーク (`CFRelease` しない) | `macos.rs:185-204` | `schedule_pump` が任意スレッドから参照するため。**CFRelease を足すと UAF** (SRV-14 参照) |

---

## §2 横断テーマ (複数サブシステムにまたがる構造問題)

### T1. シングルブラウザ前提が全層に暗黙に焼き付いている
- server: フレームペーシング state がプロセスグローバル (SRV-2)、IOSurface プールが C グローバルシングルトン (SRV-7)、Mach client port 単一 (SRV-6)
- client: `cef_unity_recv_iosurface_texture` に browser handle 引数がない (`NativeMethods.g.cs:267`)
- C#: `s_instance` singleton (CS-9)
- **方針**: 当面は「サーバー 1 プロセス = ブラウザ 1 つ」を公式制約として `CreateBrowser` 2 回目を `Response::Error` で拒否するのが最小工数で誠実。複数化要件が来たら SRV-2 の per-browser 化から着手

### T2. プロセスライフサイクルが「時間任せ」
- init: `oneshot_server.accept()` 無期限ブロック → server 起動失敗で Unity Editor が永久フリーズ (CLI-3/IPC-2)
- shutdown: fire-and-forget + 500ms 固定 sleep (CLI-14/IPC-2)
- `Child` ハンドル破棄 → ゾンビプロセス蓄積、死活監視不能 (IPC-2)
- **方針**: `ServerConnection` に `Child` を保持し、タイムアウト付き accept / try_wait ポーリングに置換 (CLI-3, IPC-2 参照)

### T3. エラーが構造的に伝わらない (silent failure)
- server: 未準備 browser で黙って Ok を返すコマンドと Error を返すコマンドが混在 (SRV-4)
- client: エラーは tmp のログファイル行き止まり、C# には -1/null/0 しか届かない (CLI-7)
- IPC: `Response::Error { msg: String }` の stringly-typed、FFI で -1 に縮退 (IPC-10)
- **方針**: ErrorCode enum の導入 (IPC-10) + `cef_unity_get_last_error` (CLI-7) をセットで

### T4. 二重実装・コピー同期 (ショットガンサージェリー)
- `drain_commands` + tick 骨格が macos.rs / generic.rs に丸ごと二重 (SRV-10/IPC-5)
- Mach メッセージ struct が server C / client ObjC に手書き二重定義 (IPC-3)
- blocking / fire-and-forget FFI 11 関数×2 の全文コピペ (CLI-5/IPC-6)
- d3d11.rs / d3d12.rs の fence・キャッシュ・Unity interface 解決の重複 (CLI-6)
- C# Interop が 2 リポジトリで手動コピー同期、既に 1188 行分岐 (CS-3)
- ログ実装が server 2 系統 + client 3 系統 (SRV-8/CLI-13)

### T5. FFI/unsafe 境界の防御欠如
- client: panic ガードなし = Editor ごと abort (CLI-1)、ハンドル UAF/double-free 無防備 + audio thread との `&mut` エイリアシング UB (CLI-2)
- server: `static mut SERVER_STATE` の `&mut` 再借用 (SRV-3)
- C#: SafeHandle 不使用、破棄順序がコメント頼み (CS-4)

---

## §3 Rust server (crates/server) の発見

### SRV-1. server.rs が 7 責務を抱える God module 【優先度: 高 / 工数: 中】
- **場所**: `crates/server/src/server.rs:1-1590`
- **現状**: ログ基盤 (46-79)、CEF ローダ (87-112)、レイテンシ計測 (138-201)、CEF ハンドラ 6 種 (203-695)、キャレット追跡 JS (511-544)、フレームペーシング (715-746, 1443-1550)、CefServer 本体 + 22 コマンドディスパッチ (748-1566)、helper パス解決 (1572-1590) が単一ファイルに同居
- **問題**: 最も繊細なフレームペーシングが単純な入力中継コードと物理的に混在し、無関係な変更がペーシングを壊すリスクが高い
- **修正案**: `logging.rs` / `cef_bootstrap.rs` / `handlers/{render,audio,lifecycle}.rs` / `caret_tracking.rs` / `frame_pacing.rs` / `commands.rs` へコード移動のみの分割
- **リスク**: `PAINT_COUNT` の参照 4 箇所 (on_paint / on_accelerated_paint / send_external_begin_frame / process_pending_flushes) がモジュール境界を跨ぐ。カウンタの意味 (software+accelerated 合算) を変えないこと

### SRV-2. フレームペーシング state がプロセスグローバルで multi-browser と構造矛盾 【優先度: 高 / 工数: 小〜中】
- **場所**: `server.rs:138` (PAINT_COUNT), `:156`, `:160`, `:164`, `:759-764` (pending_flush 単一スロット。コメント自身が単一 Browser 前提と明記)
- **問題**: 2 つ目のブラウザ作成で damage streak 誤判定・計測汚染・flush 消失が静かに発生する
- **修正案**: 最小 = `CreateBrowser` 2 回目を `Response::Error` で拒否し制約を明文化。本格 = `frame_pacing.rs` に per-browser `FrameStats` (Arc 共有、既存 viewport_w/h と同パターン)
- **リスク**: `PAINT_COUNT` は damage-streak 抑止の入力。「on_accelerated_paint で increment → 次の SendExternalBeginFrame で読む」順序を不変に

### SRV-3. `SERVER_STATE` の `static mut` 生ポインタと `&mut` 再借用 (UB の芽) 【優先度: 高 / 工数: 小】
- **場所**: `crates/server/src/event_loop/macos.rs:66, 100, 109, 170, 204`
- **問題**: CEF がネストした run loop を回すと timer_callback が再入し `&mut` 二重で UB。panic 経路でも `&mut` を再作成している
- **修正案**: (1) `CFRunLoopTimerContext.info` に `*mut ServerState` を渡す正規経路へ (現在 null で未使用)。(2) `thread_local! IN_TICK: Cell<bool>` で再入検出 + early return。(3) panic フラグは `AtomicBool` に分離
- **リスク**: tick 内の処理順序と 1ms interval には触れない (§1)

### SRV-4. ブラウザ解決ボイラープレート 12 回反復 + エラー方針の非一貫性 【優先度: 高 / 工数: 中】
- **場所**: `server.rs:1101-1441`。`load_url` (1110) は未準備で Error、`mouse_move` (1138-1152) は黙って Ok、`ime_commit_text` (1390-1411) は host 取得失敗で黙って Ok
- **問題**: `on_after_created` が非同期に slot を埋めるため「未準備ウィンドウ」が必ず存在するのに、クライアントはコマンド到達を判別できない。IME 統合テストが sleep 頼みなのはこれが根本原因
- **修正案**: `enum BrowserAccessError { NotFound, NotReady, NoFrame }` + `with_host` / `with_frame` ヘルパで解決を 1 箇所に集約。未準備を Ok にする選択は `.or_ok_if_not_ready()` のように明示化
- **リスク**: 外部から見える Response は第一段階では変えない。内部整理とプロトコル変更を別コミットに

### SRV-5. `on_accelerated_paint` の GPU 転送経路に抽象化なし 【優先度: 中 / 工数: 中】
- **場所**: `server.rs:262-392` (macOS: 272-329、Windows: 331-391)、`d3d11_pool.rs:54-69` (非 Windows スタブ)
- **問題**: 「pool へ GPU コピー → ハンドル通知 → shm に unity_frame+frame_id 書き込み」の共通プロトコルがコードに現れておらず、順序不変条件が両ブロックに別々に書かれている。macOS プールは C グローバル、Windows プールは per-browser Rust オブジェクトという非対称もある
- **修正案**: `trait GpuFramePublisher { fn publish(&self, info, w, h, format) -> Option<PublishedFrame> }` を導入し、共通シーケンス (record_latency → publish → write_unity_frame → write_info) をハンドラ側 1 箇所に。スタブ D3D11Pool も不要になる
- **リスク**: **最重要の性能実証箇所** (§1 参照)。trait 化しても呼び出しは同期のまま、Arc clone をフレームループに増やさない

### SRV-6. Mach クライアントポートの受動的ライフサイクル管理 【優先度: 中 / 工数: 小〜中】
- **場所**: `crates/server/src/mach_iosurface.c:39, 82 (early return), 154-157`
- **問題**: 接続済みだと新 subscribe を受信すらしない。旧クライアントが「生きているが受信しない」状態だと新クライアントは永久に接続不能
- **修正案**: (1) early return を削除し last-subscriber-wins (旧 port を deallocate して置換)。(2) 根本対応は `MACH_NOTIFY_DEAD_NAME` 通知。(3) `mach_ffi.rs` safe ラッパで `unsafe extern` 宣言を server.rs:20-36 から追い出す
- **リスク**: accept は毎フレーム呼び出し (server.rs:303) — 非ブロッキング (timeout=0) 維持。send timeout 10ms 変更不可

### SRV-7. IOSurface プールの所有権契約が暗黙 【優先度: 中 / 工数: 小】
- **場所**: `iosurface_pool.m:18-30, 62-76, 79-110`
- **問題**: `copy_and_get` が返す `IOSurfaceRef` は「次の呼び出しまで有効な借用」だがどこにも書かれておらず、Rust 側は生 `*mut c_void`。将来 send 非同期化などをすると resize 時の `invalidate_pool` で UAF
- **修正案**: `iosurface_ffi.rs` を新設し、`PoolSurfaceRef<'frame>` (Send/Sync 非実装) で寿命を型に落とす。C 実装には一切触れない
- **リスク**: C 側 (blit・waitUntilCompleted・POOL_SIZE) 不変。Rust ラッパ追加のみ

### SRV-8. ログ基盤 2 系統併存 (main.rs / server.rs) 【優先度: 中 / 工数: 小】
- **場所**: `main.rs:18-33` と `server.rs:46-79`。event_loop は main.rs 側を参照 (`macos.rs:90-92`)
- **問題**: event_loop からのログ (コマンド受信・panic・切断 = 一番知りたい情報) が Unity 側 `GetLogs` に載らない。フラグも 2 本
- **修正案**: `logging.rs` に単一実装 (OnceLock、フラグ 1 本、バッファ 1 本、ファイルハンドル保持)
- **リスク**: 無効時 early return を先頭に残す (hot path から呼ばれる)

### SRV-9. Mutex poisoning がシャットダウンをカスケード panic させる 【優先度: 中 / 工数: 小】
- **場所**: `macos.rs:95-105` (catch_unwind)、`server.rs:1088` (`browser.lock().unwrap()`)、`:70, :178`
- **問題**: panic が Mutex 保持中に起きると shutdown → destroy_browser で二次 panic、`cef::shutdown()` 未到達で GPU プロセス孤児化。panic payload も捨てている
- **修正案**: `lock().unwrap_or_else(PoisonError::into_inner)` に統一 + catch_unwind の payload を downcast してログ + `panic::set_hook`
- **リスク**: なし (panic 経路のみ)

### SRV-10. drain_commands / tick の完全重複 + Shutdown 制御フローのリーク 【優先度: 中 / 工数: 小】
- **場所**: `macos.rs:137-165` と `generic.rs:68-97` (ほぼ逐語的重複、既にスタイルが分岐)。`server.rs:936-939` (Shutdown は Ok を返すだけで実処理はループ側の覗き見)
- **問題**: プラットフォーム固有なのは待機/ウェイクアップ機構だけなのに、切断面が「イベントループ全体」になっている
- **修正案**: `event_loop.rs` 共通層に `tick()` / `drain_commands() -> LoopControl` を移動。`handle_command` の戻りを `enum Handled { Response(Response), ShutdownRequested }` にして shutdown 判定を dispatch 内に一元化
- **リスク**: 共通化後も両プラットフォームで tick 順序 (§1) を保証すること

### SRV-11. generic イベントループの lost-wakeup 【優先度: 中 (Windows 品質) / 工数: 小】
- **場所**: `generic.rs:14-24, 42-47`
- **問題**: tick 実行中の notify が消え、次の待機が delay いっぱい眠る。delay store も last-writer-wins で早い要求が上書きされる
- **修正案**: 世代カウンタを述語にした Condvar 待ち + delay は min を取る
- **リスク**: macOS 経路に触れない。busy-wake にならないよう generation 比較必須

### SRV-12. flush 方針・damage streak がテスト不能 【優先度: 高 (資産保護) / 工数: 中】
- **場所**: `server.rs:1465-1550`。既存テストは bundled .app 必須・#[ignore]・sleep 5 秒超・IME のみ
- **問題**: 最重要ロジックの回帰検出手段が「Editor 目視 + AFI 計測」しかない
- **修正案**: `FramePacer` 構造体として純粋ロジック抽出。`now: Instant` と `paint_count: u64` を引数注入 (trait 不要)。既存の閾値・判定式をそのまま移すだけで挙動不変、`cargo test` で全分岐検証可能に
- **リスク**: `PAINT_COUNT.load` の読み取りタイミングを 1 命令単位で保存。抽出直後にスクロール実測 (AFI +120/2s) で回帰確認

### SRV-13. CEF グローバル初期化と CefServer 構築の混線 【優先度: 低〜中 / 工数: 小】
- **場所**: `server.rs:782-850` (`init_cef(&self)` だが実体はプロセスグローバル初期化、失敗が bool と panic に分裂)、`:750` (`next_browser_id: AtomicU32` は不要な Atomic)、`main.rs:37-71` (4 連手書き引数パース)
- **修正案**: `CefRuntime` 初期化証明トークン + `Result<_, BootstrapError>`。`ServerConfig::from_args()`。api_hash → initialize の順序 (FATAL 条項) を CefRuntime 内に閉じ込める
- **リスク**: ほぼなし

### SRV-14. macOS タイマーの意図的リークが暗黙 【優先度: 低 / 工数: 小】
- **場所**: `macos.rs:185-204`
- **問題**: 「リークしているからこそ」store(null)/load 競合が UAF にならない繊細なバランスがコメントに書かれていない。誰かが CFRelease を足すと即 UAF
- **修正案**: `static TIMER: OnceLock<TimerHandle>` で「解放しない」を型で表現 + 意図をコメント明記。null store は削除
- **リスク**: **CFRelease を追加する方向の「修正」だけは絶対にしない**

---

## §4 Rust client (crates/client) の発見

### CLI-1. FFI 境界にパニックガードなし = Editor ごと abort 【優先度: 高 / 工数: 小〜中】
- **場所**: `crates/client/src/lib.rs:205` ほか全 `extern "C"`。panic 源: `:277, 366, 456` の `CONNECTION.lock().unwrap()`、`:288` の `CString::new().unwrap()`、`d3d11.rs:234` / `d3d12.rs:375` の `FENCE.lock().unwrap()`
- **問題**: edition 2024 で extern "C" 越し unwind は即 abort = Unity Editor クラッシュ・未保存シーン消失
- **修正案**: `ffi_guard.rs` に `guard(default, f)` (catch_unwind ラッパ) を用意し全エントリポイントに適用。Mutex は `unwrap_or_else(PoisonError::into_inner)` に。CLI-4 の分割時に `ffi_fn!` マクロで機械適用
- **リスク**: catch_unwind コストは非 panic 時ほぼゼロ。panic 検出後は当該ブラウザ無効化フラグを立てる

### CLI-2. ハンドルの `&mut` エイリアシング (audio thread × main thread) + UAF/double-free 無防備 【優先度: 高 / 工数: 中】
- **場所**: `lib.rs:131-133` (`handle_to_ref` が無条件 `&mut`)、`:454, 879` (`Box::from_raw`)、`:834-844` (`read_audio` = audio thread)、`:737-739` (`get_buffer` = main thread)
- **問題**: 同一インスタンスへの `&mut` が 2 スレッドで同時に存在 (現状 disjoint フィールドで実害なしだが aliasing UB)。destroy は即時 Box 解放なので audio thread 実行中の destroy で UAF、二重 destroy で double-free
- **修正案**: `browser_handle.rs` — `shm: Mutex<ShmReader>` / `audio_shm: Mutex<Option<AudioShmReader>>` のスレッドドメイン別 Mutex + `Arc` ハンドル (destroy = decrement で audio thread 読み取り完了まで生存)。ShmReader カーソルの `AtomicU64` 化でロックレス化も選択肢
- **リスク**: `peek_accel_frame_id` → flush → recv の double-pump 列にロックを挟まない。native audio 実装 (設計済み・未実装) の**前**にやると手戻り最小

### CLI-3. `cef_unity_init` が無期限ブロックし得る 【優先度: 高 / 工数: 小】
- **場所**: `lib.rs:268` (`oneshot_server.accept()` タイムアウトなし)、`:257` (`Ok(_child)` で Child 破棄)
- **問題**: server 起動途中クラッシュ (codesign 不備・framework 欠落・CEF 初期化失敗) で Unity main thread が永久フリーズ
- **修正案**: accept を別スレッド + `recv_timeout(10s 以上)`。`Child` を保持し `try_wait()` で早期死亡検知 → エラーコード -6/-7。Child は `ServerConnection` に持たせ死活監視 (CLI-7) にも使う
- **リスク**: 低。初期化パスのみ

### CLI-4. lib.rs 1614 行の責務混在 (6 層 1 ファイル、配置も追加順) 【優先度: 中 (他の受け皿として先行価値高) / 工数: 中】
- **場所**: `lib.rs:1-1614` 全体
- **修正案**: `paths.rs` / `connection.rs` / `browser_handle.rs` / `logging.rs` / `ffi/{lifecycle,browser,input,ime,audio,texture_macos,texture_windows,logs}.rs` / `gpu/` へ分割。csbindgen は `input_extern_file` 複数指定で対応 (build.rs 変更を忘れると **C# バインディングから関数が黙って消える**)
- **検証条件**: リファクタ後に `NativeMethods.g.cs` の diff が空であること

### CLI-5. blocking / fire-and-forget 11 関数×2 の全文コピペ 【優先度: 中 / 工数: 中 (機械的)】
- **場所**: `lib.rs:466-672` (fire-and-forget 群) と `:874-1076, 1422-1444` (blocking 群)。例: `send_key_event` (597-630) と `_blocking` (1042-1076) は Command 構築まで完全同文
- **修正案**: `fn dispatch(handle, blocking: bool, build: impl FnOnce(u32) -> Command) -> i32` の単一コア。FFI 側は 1 行に。IPC-6 の Command 構築関数共通化と同時に
- **リスク**: `send_external_begin_frame` (lib.rs:1156) だけは dispatch 経由でも命令列が増えないことを確認

### CLI-6. d3d11.rs / d3d12.rs / recv 系の三重重複 + 乖離した `IUnityInterfaces` 定義 【優先度: 中 / 工数: 中】
- **場所**: `d3d11.rs:53-62` と `d3d12.rs:51-56` — **同じ Unity ヘッダ構造体の異なる Rust 定義** (値渡し vs u64×2)。`d3d11.rs:86-118`/`d3d12.rs:95-129` (Opened/Fence state 重複)、`d3d11.rs:259-333`/`d3d12.rs:397-459` (open_or_cached 重複)、`lib.rs:1528-1567`/`:1575-1614` (recv 同型)
- **問題**: 将来どちらかで `get_interface` を呼んだ瞬間に片方だけ ABI 破壊という罠が既に埋まっている。fence の単調性ロジックが 2 重管理
- **修正案**: `gpu/unity_plugin_api.rs` (定義 1 本化)、`gpu/opened_cache.rs` (`OpenedCache<R>` ジェネリクス)、`gpu/fence.rs` (`FenceGate<F>`)、`trait GpuBackend` で `recv_gpu_texture::<D3D11>()` に統合
- **リスク**: `wait_fence` → `open_or_cached` 順序と `last_waited` 単調スキップ条件 (§1)。D3D12 の `declare_initial_state` は固有フックとして残す

### CLI-7. silent failure — エラーが C# に一切伝わらない 【優先度: 中 / 工数: 中】
- **場所**: `lib.rs:187-192` (`let _ =` で送信エラー破棄)、`:360, 473, 905` (`to_str().unwrap_or("")` で不正 URL が空文字列として送信)、`:1548-1550` (fence 失敗をログのみで続行)、`:692-711` (get_url の「URL なし」と「IPC 断」が同じ 0)
- **修正案**: `error.rs` — `LAST_ERROR` + `cef_unity_get_last_error(buffer, len)` (get_logs と同じ 2 フェーズ規約)。`SERVER_ALIVE: AtomicBool` + `cef_unity_is_server_alive()`。`unwrap_or("")` は return + last_error 記録に
- **リスク**: set_last_error は失敗時のみ実行なので hot path 影響なし

### CLI-8. `CONNECTION` 単一 Mutex による全 FFI 直列化 + 応答相関の暗黙契約 【優先度: 中 / 工数: 中】
- **場所**: `lib.rs:156, 176-184, 687-698`
- **問題**: blocking コマンドの応答待ち中、全入力イベント・BeginFrame 送信がロック待ち (head-of-line blocking)。相関保証は「Mutex を握ったまま send→recv」という暗黙契約のみ。C# が別スレッドから 1 つでも呼ぶと 60fps 経路が blocking IPC の後ろに並ぶ
- **修正案**: `connection.rs` で `request(&mut self)` / `post(&self)` に分離。`IpcSender` は clone 可能なので fire-and-forget 用 clone を Mutex 外に配布。IPC-1 の seq 相関導入とセットで
- **リスク**: 複数スレッド post の到着順は未定義になるため、BeginFrame 系 main thread のみの呼び出し規約を doc + デバッグアサートで固定してから

### CLI-9. `read_audio` のスライス構築が契約次第で即 unsound 【優先度: 中 / 工数: 小】
- **場所**: `lib.rs:841-843` — 常に `max_frames * AUDIO_MAX_CHANNELS` 幅で `from_raw_parts_mut`。doc (817-820) は「推奨」としか言っていない
- **問題**: C# がステレオ前提で `max_frames * 2` しか確保しないと、スライス構築時点で確保外を含む `&mut [f32]` = UB (Miri 検出対象)
- **修正案**: ipc crate に `read_into_ptr(out: *mut f32, max_frames) -> (usize, usize)` を追加 (スライスを作らず実チャネル数分だけ ptr::write)。doc は「必須」に格上げ。**native audio A 案実装前に直すのが安い**
- **リスク**: 低。read カーソルのセマンティクス (録画 tap と native 再生の独立カーソル) には触れない

### CLI-10. Mach port のライフサイクル欠落 — 再 init で port リーク 【優先度: 中 / 工数: 小】
- **場所**: `metal_texture.m:44, 59-64` (connect のたび新規 allocate、旧 port 解放なし)、`lib.rs:323-343` (shutdown が Mach 側を片付けない)
- **問題**: Editor の Play/Stop 繰り返しで receive port が 1 個ずつリーク + `_surfaceCache` が前セッションの IOSurfaceRef を保持 (1920×1080 で約 8MB×4)。**Editor 5h+ 劣化調査の計測ノイズ源の可能性**
- **修正案**: `mach_iosurface_client_disconnect()` を追加 (port 破棄 + cache 全 CFRelease + srgbView nil)。`cef_unity_shutdown` から呼ぶ。connect 側に「既接続なら先に disconnect」ガード
- **リスク**: **drain-latest recv ループ (metal_texture.m:117-147) には触れない**

### CLI-11. legacy Metal 経路の `@autoreleasepool` 欠如 + 死に体 ABI 残存 【優先度: 中 (pool 修正は即時可) / 工数: 小】
- **場所**: `metal_texture.m:231-265` (`cef_unity_create_metal_texture_objc` — pool なしで autoreleased MTLTextureDescriptor 生成)、`lib.rs:1225-1244, 1293-1308`
- **問題**: プロジェクトの既知知見 (「Rust→ObjC の Metal 生成は必ず pool で囲む」) 違反。macOS 16 で壊れているとコメント済みの API が ABI に公開され続けている
- **修正案**: 短期 = 本体を `@autoreleasepool` で囲む (1 行)。中期 = C# から未使用確認の上 csbindgen 対象から外して削除。`_sharedDevice` 遅延初期化 2 箇所 (152-160, 239-242) を `ensure_device()` に集約
- **リスク**: pool 追加はゼロリスク。ABI 削除は C# 2 プロジェクト再生成 + grep 確認必須

### CLI-12. D3D11 immediate context の呼び出しスレッド未規定 【優先度: 中 / 工数: 中 (段階 1 は小)】
- **場所**: `d3d11.rs:116` (`unsafe impl Send`)、`:230-249` (immediate context に Wait 発行)、`lib.rs:1548`
- **問題**: immediate context は非スレッドセーフ COM。C# が main thread から呼ぶと Unity render thread と競合 → まれな DEVICE_REMOVED 系。Windows 経路固有 (D3D12 の CommandQueue::Wait はスレッドセーフ)
- **修正案**: 段階 1 = 呼び出しスレッド規約の doc 明記 + debug ビルドでスレッド ID 記録アサート。段階 2 = `cef_unity_get_render_event_func()` エクスポートで render thread コールバックに閉じ込め (Unity ネイティブプラグイン標準パターン)
- **リスク**: 段階 2 は C# フロー変更 + fence 実証の再検証が必要。まず段階 1 で実測してから

### CLI-13. `LOG_ENABLED` が d3d11/d3d12 に効かない 【優先度: 低 / 工数: 小】
- **場所**: `d3d11.rs:28-37`、`d3d12.rs:37-46` (マスターフラグ無視で常にファイル open + 書き込み)
- **問題**: fence 失敗継続の異常系で毎フレームのファイル open = フレームスパイク源 (macOS 側で実測排除した問題と同型)
- **修正案**: CLI-4 の `logging.rs` に 1 本化、ファイルハンドル保持
- **リスク**: なし

### CLI-14. shutdown の固定 500ms sleep 【優先度: 低 / 工数: 小】
- **場所**: `lib.rs:336`
- **問題**: Unity main thread が毎回 500ms 固まる。低速環境では逆に足りず、次 init で bootstrap 名/shm flink 衝突の可能性
- **修正案**: CLI-3 で Child 保持後、`try_wait()` を 10ms 刻み最大 2s ポーリング。または server が Shutdown 完了 ACK を返す設計 (server 側変更とセット)
- **リスク**: `drain_commands` の `expects_response=false` 契約に触れる場合は server 側とセットで

### CLI-15. キャッシュ 4 vs プール 5 の暗黙結合 【優先度: 低 / 工数: 小】
- **場所**: `metal_texture.m:15` (CACHE_SIZE=4) vs server POOL_SIZE=5
- **修正案**: **値は一切変えず**、ipc crate に `pub const IOSURFACE_POOL_SIZE` を置き、client は build.rs の `cc::Build::define` で同じ出所から注入。または設計意図をコメントで固定
- **リスク**: 値を 1 でも動かすと実証済みバランスが崩れる可能性。変更は Editor 再起動後の再計測とセットでのみ

### CLI-16. build.rs の兄弟リポジトリハードコード出力 + rerun 宣言ゼロ 【優先度: 低 / 工数: 小】
- **場所**: `crates/client/build.rs:22-35`
- **問題**: crate 単体チェックアウト/CI でビルド不能。**MEMORY 記載の「metal_texture.m 変更後 cargo clean 必要」問題は `cargo:rerun-if-changed=src` の明示宣言で解消できる可能性が高い**
- **修正案**: 出力先不在なら `cargo:warning` でスキップ + `rerun-if-changed=src` (個別列挙は宣言漏れリスクがあるためディレクトリ指定が安全)
- **リスク**: 明示宣言後は宣言したものだけが監視対象になる点に注意

---

## §5 IPC (crates/ipc + プロトコル横断) の発見

### IPC-1. 要求-応答相関が「暗黙のロックステップ」のみ — 1 回ずれると永続デシンク 【優先度: 高 / 工数: 中】
- **場所**: `crates/ipc/src/lib.rs:110-134` (CommandEnvelope/Response に相関 ID なし)、`client/src/lib.rs:176-184`、`server/src/event_loop/macos.rs:137-165`
- **問題**: `expects_response: bool` は送信側の自己申告で、両端の解釈が 1 箇所食い違うと応答キューが 1 個ずれて以後**全部**前のコマンドの応答を受け取る。自己修復不能。`recv()` タイムアウトなしで server ハング時に Unity main thread 永久凍結
- **修正案**: `seq: u64` を CommandEnvelope に追加、`ResponseEnvelope { seq, response }` で包む。client は `try_recv_timeout` + seq 不一致は読み捨て再 recv (自己修復)。送信/受信ロックを分離 (CLI-8 とセット)
- **リスク**: bincode ワイヤ表現変更 = client/server **同時デプロイ必須** (deploy.sh が両方更新するので運用上は満たされる)。映像は shm/Mach 経路なので 60fps 影響なし

### IPC-2. 接続ライフサイクルの構造化欠如 (T2 の本体) 【優先度: 高 / 工数: 中】
- **場所**: `client/src/lib.rs:246-274, 329-337`、`server/src/main.rs:104-112` (expect 連発 bootstrap)
- **問題**: ゾンビプロセス蓄積 (wait しない子プロセス × Editor 長寿命)、起動失敗と起動遅延を区別不能、server 死亡後も `CONNECTION` が None にならず全呼び出しがだらだら失敗
- **修正案**: CLI-3 + CLI-14 に加え、`send_command` Err 検出で `CONNECTION = None` 化する `with_connection` ヘルパ (現在 15 箇所に散る lock パターンが集約先)
- **リスク**: shutdown の wait タイムアウトは現行 500ms 以上に (早く殺しすぎ防止)

### IPC-3. Mach ワイヤフォーマットが crate 外で手書き二重定義 【優先度: 中 / 工数: 小】
- **場所**: `server/src/mach_iosurface.c:18-33` と `client/src/metal_texture.m:28-41` (同一 struct の再定義、msgh_id 'IOSF'/'SUBS')
- **問題**: 片側だけフィールド追加するとコンパイルは通るのに受信側がずれたオフセットを黙って読む (Mach はサイズ検証が緩い)
- **修正案**: 共有ヘッダ `crates/ipc/include/cef_unity_mach_protocol.h` を新設、両 build.rs から `-I` 参照。バイナリレイアウト不変の純移動
- **リスク**: `#pragma pack` を追加しないこと (現行は自然アライメント前提)

### IPC-4. プロトコルバージョンハンドシェイクなし 【優先度: 中 / 工数: 小】
- **場所**: `ipc/src/lib.rs:136-143` (Bootstrap に server_pid のみ)
- **問題**: bincode の enum は序数エンコードなので variant 挿入で全メッセージの意味が変わる。「deploy.sh 忘れで stale binary」という既知の運用リスクが「意味不明なエラー」として現れる
- **修正案**: `PROTOCOL_VERSION: u32` を Bootstrap に追加、不一致は専用エラーコードで即 return。`ShmHeader` は 128B 中実 92B なので**末尾に** `layout_version` を追加してもオフセット不変
- **リスク**: 導入コミット自体が同時デプロイ必須 (今も常に真なので追加リスクなし)

### IPC-5. → SRV-10 に統合 (drain_commands 二重実装)

### IPC-6. 新メッセージ追加のショットガンサージェリー (Rust 4-5 箇所 + C# 4 箇所) 【優先度: 中 / 工数: 中】
- **場所**: `ipc/src/lib.rs:15-108` (Command enum、全 variant が browser_id を個別保持)、`server.rs:853-941` (20 分岐 match)、client の二重 FFI (CLI-5)
- **修正案**: (案 1) `Command::Global(GlobalCommand)` / `Command::Browser { browser_id, cmd: BrowserCommand }` に分割 — server の browser 解決を 1 箇所化 (SRV-4 と相乗)。(案 2) Command 構築関数を blocking/no-wait で共通化 (CLI-5)。案 2 が先 (ワイヤ不変)、案 1 は IPC-1 のワイヤ変更に同梱
- **リスク**: 案 1 は bincode 表現変更 = 同時デプロイ

### IPC-7. 映像 double-buffer にティアリング検出なし (seqlock 不在) 【優先度: 中 / 工数: 小】
- **場所**: `ipc/src/lib.rs:538-558` (write_frame)、`:682-705` (read_frame — コピー後の再検証なし)、`:659-678` (get_active_buffer_ptr)
- **問題**: reader のコピー中に writer が 2 フレーム進むと新旧混在ピクセル。software 経路は Windows の現行本番経路なので実害があり得る
- **修正案**: read_frame に seqlock — コピー前後で `(frame_id, active_buffer)` を照合、変化していたら **1 回だけ**リトライ。ヘッダ変更・writer 変更不要。get_active_buffer_ptr は doc に「writer 2 フレームで invalid」を明記
- **リスク**: リトライは有限 (1 回) 固定。無限リトライは高負荷時に reader スピン → 60fps 破壊

### IPC-8. ipc/lib.rs の 3 責務同居 (実装 707 行 + テスト 700 行) 【優先度: 低 (単独では) / 工数: 小】
- **場所**: `ipc/src/lib.rs` — プロトコル (1-148)、映像 shm (150-206, 448-706)、音声 shm (208-446)、テスト (708-1407)
- **修正案**: `protocol.rs` / `video_shm.rs` / `audio_shm.rs` に分割、lib.rs は pub use のみ (外部 API 不変)。**IPC-1/4/6 をやるなら先にこれ** (後続 diff が読みやすくなる)
- **リスク**: ほぼゼロ

### IPC-9. shm 読み出しの u32 乗算オーバーフロー 【優先度: 低 / 工数: 小】
- **場所**: `ipc/src/lib.rs:670, 693` (`(width * height * 4) as usize` — u32 内で wrap し境界チェックをすり抜け得る)
- **修正案**: `(width as usize) * (height as usize) * 4` に統一 (3 箇所) + `width <= MAX_W && height <= MAX_H` の明示チェック
- **リスク**: なし

### IPC-10. Response::Error の stringly-typed → FFI で -1 に縮退 【優先度: 低 / 工数: 小〜中】
- **場所**: `ipc/src/lib.rs:133`、`client/src/lib.rs:858-871` (blocking_simple)
- **修正案**: `Response::Error { code: ErrorCode, msg }` + `#[repr(i32)] enum ErrorCode { Generic = -1, BrowserNotFound = -2, ShmCreateFailed = -3, CefError = -4, ... }`。blocking_simple は code をそのまま FFI 戻り値に (C# の「負値=失敗」判定は無修正で動く)
- **リスク**: bincode 表現変更 (同時デプロイ)。server.rs の Error 構築 ~20 箇所を触る

---

## §6 Unity C# 層の発見

### CS-1. 「Sample」という名前の 1363 行 God Class が製品本体 【優先度: 高 / 工数: 大】
- **場所**: `Assets/CefUnity/Runtime/CefUnityBrowserSample.cs:22` — ライフサイクル (239-285, 431-464)、PlayerLoop フック (663-742)、0F 同期ステートマシン (533-649)、入力 (911-1118)、IME (793-893, 1327-1362)、テクスチャ (1141-1270)、音声 (759-791)、計測 (107-237, 317-402) の 8 責務
- **問題**: 利用者は 1363 行をコピーしないとブラウザを表示できない。IME 座標系・damage streak 推定・PlayerLoop アンカーという「二度と書きたくない知見」が再利用不能。`Assets/Scripts/Sample.cs` が別にあり「サンプルのサンプル」状態
- **修正案** (依存の少ない順): `CefKeyboardMapper` (static 純関数) → `CefZeroFramePacer` (**UnityEngine 非依存の純 C# 化 + EditMode テスト** — 最重要ロジックが現在テスト不能) → `CefPlayerLoopHooks` → `CefBrowserInput` (+`IBrowserCoordinateMapper`) → `CefImeHandler` → `CefTexturePresenter` → `CefBrowserView` (100 行以下の束ね役 MonoBehaviour)。Sample は `Samples~/` へ。診断系は `CefDiagnostics` に隔離
- **リスク**: 0F 待ちロジック (FreshPaintMinDelayMs=4.5 等) と EarlyUpdate→PostLateUpdate 順序 (§1)。IME は GameView Scale 補正・Y 反転不要の座標系が壊れやすい。分割ごとにスクロール実証を再測定

### CS-2. Runtime asmdef に無条件 `using UnityEditor;` — 実機ビルド不能 【優先度: 高 (ブロッカー) / 工数: 小】
- **場所**: `CefUnityBrowserSample.cs:6` (本体 1327-1362 は #if ガード済みだが using が未ガード。asmdef は全プラットフォーム対象)
- **修正案**: 即応 = using を `#if UNITY_EDITOR` で囲む。設計 = GameView Scale 反射を Editor asmdef 側 `EditorGameViewScaleProvider` に移し、Runtime は `static Func<float> ScaleProvider` (既定 1f) に `[InitializeOnLoadMethod]` で注入
- **リスク**: ドメインリロード後の再注入を忘れると Scale≠1 の GameView で IME キャレットがずれる

### CS-3. Interop の 2 リポジトリ手動コピー同期 — 既に 1188 行分岐 【優先度: 高 / 工数: 中】
- **場所**: `Assets/CefUnity/Interop/` vs `cef-unity-csharp/Interop/`
- **事実確認済み**: NativeMethods.g.cs は完全一致 (コピー)。CefUnity.cs は diff 1188 行で双方向ドリフト (csharp 側のみ `ConvertBgraToRgba`/非ブロッキング `ExecuteJavaScript`、Unity 側のみ `TryRecvD3D11Texture` 等)。namespace も `Interop` vs `CefUnity.Interop` で別
- **問題**: FFI シグネチャ変更が片側だけ反映されると、コンパイルは通るのに実行時スタック破壊・マーシャリング不整合
- **修正案**: (1) csbindgen 出力先を Unity 側に一本化 (Rust build.rs のパス変更、または deploy.sh にコピー組み込み)。(2) 手書きラッパーは Unity 側を single source とし、csharp 側 Sandbox は csproj の `<Compile Include="../..../Interop/**/*.cs" />` でファイル参照。(3) UPM 完成 — `package.json` (d411c1f で雛形追加済み、`jp.juha.cefunity` v0.0.0) に unity/author/samples を追記し `Packages/` へ移動。`Script.asmdef` → `CefUnity.Runtime.asmdef` にリネーム
- **リスク**: パッケージ移動は Plugins 内 dylib/server.app の meta GUID 保持と LFS 設定 (462b6e9 で調整済み) を巻き込む。CS-8 のパス解決も同時修正が必須

### CS-4. Browser ハンドルが生ポインタ + finalizer なし 【優先度: 中 (配布前提なら高) / 工数: 中】
- **場所**: `Assets/CefUnity/Interop/CefUnity.cs:221-253, 801-804`、破棄順序契約は `CefUnityBrowserSample.cs:440-448`
- **問題**: Dispose 漏れ・Start 中例外・ドメインリロードで native browser/server が孤児化する最後の砦がない。ReadAudio と Dispose の並行は use-after-free (防御はコメントのみ)
- **修正案**: `CefBrowserHandle : SafeHandleZeroOrMinusOneIsInvalid` (csbindgen 署名を変えられないなら `DangerousAddRef/Release` 定型)。`CefRuntime` に init 参照カウント + `AssemblyReloadEvents.beforeAssemblyReload` / `Application.quitting` フック。`ThrowIfDisposed` は `ObjectDisposedException` に
- **リスク**: native destroy はブロッキング (音声排水待ち設計) — リロードフックからの呼び出しが Editor をハングさせないか要確認

### CS-5. 音声 3 クラス — 骨格は良好、Output だけ 3 役 【優先度: 中 / 工数: 中】
- **場所**: `CefAudioOutput.cs` (330 行中 122-220 の約 120 行が診断)
- **問題**: producer がメインスレッド Update() ポーリング固定に密結合しており、A 案 (audio-thread pull) / CRI 方式 (native 出力) へ差し替えられない
- **修正案**: 診断を `CefAudioDiagnostics` に分離。producer を `ICefAudioSource` (`Pull(buf, maxFrames, out ch)`) に抽象化 — `MainThreadPollingSource` (現行) / `AudioThreadPullSource` (A 案) / native 時 no-op。TryInitStream は `CefAudioStreamNegotiator` に独立
- **リスク**: audio-thread pull 化は FFI をオーディオスレッドから呼ぶ = CLI-2/CLI-9 の修正が前提

### CS-6. オーディオスレッド境界 — Configure 非アトミック公開 + lock ベース Ring 【優先度: 中 / 工数: 中】
- **場所**: `CefAudioSink.cs:39-60, 88-134`、`CefAudioRing.cs:30, 79-182`
- **問題**: (1) Configure 中に DSP コールバックが走ると「新 _ring + 旧 _srcChannels」の引き裂かれ構成 (チャネル数変更の再構築経路 `CefAudioOutput.cs:297-303` で実際に到達可能)。(2) オーディオスレッドの lock 取得は priority inversion の古典パターン — DSP 量子を詰めるほど顕在化
- **修正案**: immutable `SinkConfig` 一式を `volatile` 参照でアトミックスワップ (コールバック冒頭で 1 回ローカルコピー)。Ring は SPSC 前提で `Volatile.Read/Write` + Interlocked による lock-free 化 (既存 `CefAudioRingTests` が回帰テストとして機能)
- **リスク**: `_readFrame` が double である点 (64bit アトミック性はあるが順序性なし)。テストの不連続検出 (maxDiscontinuity) を必ず通す

### CS-7. 計測用 temp ファイルトグルが本体制御フローに編み込み 【優先度: 低 (CS-1 と同時なら安い) / 工数: 小】
- **場所**: `CefUnityBrowserSample.cs:255, 293-315, 488-493` (毎フレーム `File.Exists`×最大 4 = syscall)、`:1133-1234`、`:115-204` (計装フィールド約 40 個)
- **修正案**: `CefPerfHarness` (`#if CEF_UNITY_PERF_TEST`) に集約、ファイルチェックは起動時 1 回 + 1 秒間隔ポーリング。本体には `Pacer.ForceDisableWait` 等のフラグ注入点のみ残す
- **リスク**: 「実行中 PlayMode へ temp ファイルで指示を渡す」計測ワークフローのファイル名互換を保つこと

### CS-8. CefBuildPostProcessor のパスハードコードが UPM 化と衝突 【優先度: 中 (CS-3 の前提条件) / 工数: 小】
- **場所**: `Assets/CefUnity/Editor/CefBuildPostProcessor.cs:45-47, 90-92` (`Application.dataPath + "CefUnity/..."` 固定)
- **問題**: Packages/ へ移動した瞬間、ビルド後処理が静かに失敗して実機で server 不在クラッシュ
- **修正案**: `PackageInfo.FindForAssembly(typeof(CefBuildPostProcessor).Assembly)` の `resolvedPath`、または既知アセット GUID から解決。Assets 直置きと Packages 配置の両対応に
- **リスク**: `.app` バンドルの実行権限・symlink は `File.Copy` ベースで失われる可能性 (現状も潜在) — 署名済み framework のコピーは `ditto` 使用を検討

### CS-9. シングルブラウザ前提の static 焼き付き (→ T1) 【優先度: 低 / 工数: 小 (C# 層のみ)】
- **場所**: `CefUnityBrowserSample.cs:207` (s_instance)、`Interop/CefUnity.cs:455-465` (TryRecvIOSurfaceTexture が static)、`NativeMethods.g.cs:267` (handle 引数なし)
- **修正案**: C# 側は `CefPlayerLoopDriver` (static) が `List<CefBrowserView>` を回す構造へ (CS-1 に含める)。FFI 側の handle 追加は Rust 側 TODO として明記に留める
- **リスク**: Mach port はプロセス単位 1 本のため、native 変更なしに C# だけ複数対応すると 2 つ目のブラウザの絵が混線する。C# 層は「構造だけ」複数可能に

### CS-10. `useGpu` 判定が計算されたまま未使用 【優先度: 低 / 工数: 小】
- **場所**: `CefUnityBrowserSample.cs:258-262` — `var useGpu = !(D3D12 || D3D11);` を計算直後、Init に渡していない (既定 useGpu=true)
- **問題**: 「Windows で software に落とす意図」か「消し忘れ」か判別不能。現挙動は常に GPU 経路要求
- **修正案**: 意図が「常に GPU」なら変数削除。プラットフォーム別に落とす必要が残るなら Init に渡して SerializeField でオーバーライド可能に

---

## §7 推奨ロードマップ (サブシステム横断)

依存関係とリスクを考慮した統合順序。各フェーズは独立にマージ可能。

### Phase 0: ブロッカー・安全性 (小粒・即効)
1. **CS-2** using UnityEditor ガード (実機ビルド不能の解消、数行)
2. **CLI-11** legacy Metal 経路への `@autoreleasepool` 追加 (1 行)
3. **CLI-3** init タイムアウト + Child 保持 (Editor 永久フリーズ防止)
4. **CLI-1** ffi_guard (Editor abort 防止)
5. **SRV-3** SERVER_STATE の正規化 (UB の芽)
6. **SRV-9** Mutex poisoning 対策 + **IPC-9** u32 オーバーフロー + **CS-10** useGpu

### Phase 1: 分割の受け皿 (純コード移動、ワイヤ不変)
7. **IPC-8** ipc crate モジュール分割
8. **SRV-10** drain_commands/tick 共通化 + **IPC-3** Mach ヘッダ共有
9. **SRV-1** server.rs 分割 + **SRV-8** ログ統合
10. **CLI-4** lib.rs 分割 + **CLI-13** ログ統合 (検証: NativeMethods.g.cs diff ゼロ)

### Phase 2: 最重要ロジックの資産保護
11. **SRV-12** FramePacer 純粋化 + ユニットテスト (→ スクロール実測で回帰確認)
12. **SRV-2** 単一ブラウザ制約の明文化 (最小案)
13. **CS-1** God Class 分割 — まず `CefZeroFramePacer` 純 C# 化 + EditMode テストから
14. **SRV-4** with_host/with_frame (内部整理のみ、Response 意味論は不変)

### Phase 3: soundness と重複除去
15. **CLI-2** ハンドル/audio の soundness + **CLI-9** read_audio (← **native audio A 案実装の前提**)
16. **CLI-5** dispatch 共通化 + **IPC-6 案 2** Command 構築関数共通化
17. **CLI-6** gpu/ 統合 (fence セマンティクス不変をレビュー観点に)
18. **CS-6** SinkConfig アトミックスワップ + Ring lock-free 化

### Phase 4: ワイヤプロトコル変更 (1 コミットに束ねて同時デプロイ 1 回)
19. **IPC-1** seq 相関 + **IPC-4** バージョンハンドシェイク + **IPC-6 案 1** Browser/Global 分割 + **IPC-10** ErrorCode
20. **CLI-7** get_last_error + **CLI-8** 送受信ロック分離 (IPC-1 とセット)
21. **CLI-14** shutdown ACK 化 + **IPC-2** ライフサイクル完成

### Phase 5: エコシステム整備
22. **CS-3** Interop 単一ソース化 + **CS-8** PostProcessor パス解決 + UPM 完成
23. **CS-4** SafeHandle + リロードフック
24. **CS-5** ICefAudioSource 抽象化 (native audio 実装の直前に)
25. 残り: SRV-5/6/7/11/13/14, CLI-10/12/15/16, IPC-7, CS-7/9

---

## §8 回帰確認プロトコル

修正の種類に応じて実施すること。

### 全リファクタリング共通
- `cargo build` (workspace) + `cargo test -p cef-unity-ipc`
- FFI 署名に触れた場合: 再生成後の `NativeMethods.g.cs` diff がゼロであること (署名不変のリファクタの場合)
- ObjC (.m) を触った場合: `cargo clean -p cef-unity-client --release` してからビルド (.o キャッシュ問題)
- deploy は `cef-unity-rust/` から `deploy.sh` (release のみ)

### 性能実証箇所 (§1) に近い修正
1. **Unity Editor を再起動する** (5h+ 稼働で CEF 20-30fps 劣化の計測の罠)
2. dylib 変更後も Editor 再起動必須 (一度ロードすると保持される)
3. スクロール実測: AFI +120/2s (CEF 内部 60fps)、recv 120/120、std ~0.65ms
4. 音声を触った場合: アンダーラン 0 の確認、`CefAudioRingTests` パス。遅延検証は内蔵スピーカー/有線で行う (BT の WF-C700N は単体 219ms)

### ワイヤプロトコル変更 (Phase 4)
- client dylib と server バイナリの同時デプロイ必須
- 旧バイナリ混在時に「明示的なバージョンエラー」になることを確認 (IPC-4 導入後)

### IME 関連
- 連続 IME 入力 (「夏目」確定 →「漱石」入力) で候補ウィンドウ位置を確認
- Editor GameView Scale を 2x にしてキャレット座標を確認 (Y 反転は不要が正)
