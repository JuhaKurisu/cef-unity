# Unity 抜き harness による issue #7〜#15 の再現・計測

- 計測日: 2026-07-30
- 環境: Apple M3 / macOS 26.2 / 1920x1080 viewport / CEF 145.5.0
- 対象: [issue #7〜#15](https://github.com/JuhaKurisu/cef-unity/issues)（`docs/MOORESTECH_PR1097_VERIFICATION.md` にもとづき起票したもの）
- 手段: `CefUnity.Harness` の診断コマンド + server 側の STATISTICS 計装。**Unity Editor は一切使わない**

## 使い方

```bash
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0

# GPU 経路の paint 供給と server の pump 健全性 (#7 / #12 / #13 / #14)
CefUnity.Harness paint-statistics [秒] [幅] [高さ] [animation|small-damage]

# 0F 待ち busy-wait の代償と効果 (#11)
CefUnity.Harness zero-frame-wait [秒] [zeroFrameWaitMilliseconds] [幅] [高さ] [animation|intermittent]

# Play/Stop 相当のサイクル (#8 / #9 / #10)
CefUnity.Harness lifecycle [サイクル数] [listenPort] [1 サイクルのフレーム数]
```

競合条件は `cef-unity-rust/tools/gpu-contention-hog.m` をビルドして `hog` を複数本起動し、
CPU 飽和は busy loop を ncpu 本走らせて作る。

server 側のトグル（すべて環境変数、既定は従来動作）:

| 変数 | 効果 |
|---|---|
| `CEF_UNITY_ASYNC_COPY=1` | GPU コピーを非同期化（#7 の修正候補） |
| `CEF_UNITY_UNSAFE_NO_WAIT=1` | 完了を待たず即送信（**既知不良**。検出器の negative control 専用） |
| `CEF_UNITY_PUMP_INTERVAL_MILLISECONDS=n` | pump fallback 間隔（既定 1、#12 の A/B 用） |

## 結果一覧

| issue | 再現 | 実測 |
|---|---|---|
| #7 P0 同期 GPU コピーが pump を止める | ✅ | copy_wait が実時間の 50〜67%、単発最大 **356ms**、pump 1000→43〜333 ticks/s |
| #8 P0 shutdown が server を回収しない | ✅ | 競合下で **5 サイクル中 2 回**、Shutdown 復帰後も server が存命 |
| #9 P1 server が親の FD を継承する | ✅ (条件付き) | CLOEXEC を外した LISTEN ソケットが同一 fd 番号で server に出現 |
| #10 P1 Mach port / キャッシュのリーク | ✅ | Mach port が **+1〜2/サイクル**で単調増加、surface キャッシュは上限 4 まで蓄積 |
| #11 P1 メインスレッドの busy-wait | ✅ | 間欠ページで spin が実時間の **42.5%**・CPU 約 25 倍、0F 達成 69% |
| #12 P1 1000Hz 固定 pump | ✅ | 間隔 1→16ms で **paint 60/s・レイテンシ・0F 達成率は不変**、CPU 120→85ms/s |
| #13 P2 BeginFrame の burst drain | △ 部分的 | 競合下でも burst 最大 **1〜4**。規模は pump 停止時間に比例するので #7 の下流 |
| #14 P2 dirty rect 未使用 | ✅ | 小 damage ページで damage 面積は転送面積の **0.0〜3.3%** |
| #15 P3 VERBOSE ログ常時出力 | ✅ | `--logging` OFF でも `cef_debug.log` が約 10 秒で **749KB** |

## #7 — 同期 GPU コピーが CEF pump を止める

`on_accelerated_paint` と event loop の tick はどちらも `ThreadId(1)`（同一スレッド）であることを
ログで直接確認した。したがってコピー完了待ちはそのまま pump の停止時間になる。

| 条件 | pump_ticks/s | copy_wait_total/s | copy_wait_max | client received/s |
|---|---|---|---|---|
| 競合なし | ~1000 | 42〜153ms (4〜15%) | 2〜24ms | 58〜61 |
| GPU 競合 | 459〜780 | 260〜665ms | 13〜47ms | 51〜59 |
| GPU + CPU 飽和 | 43〜499 | 316〜666ms (32〜67%) | **356ms** | 33〜54 |

- **競合が無くても実時間の 4〜15% は `waitUntilCompleted` で寝ている**
- 単発 300ms 超は「GPU 競合だけ」では起きず、**待機スレッドの CPU 追い出しが加わって初めて**再現する
- Mach 送信 (`mach_msg`、10ms timeout の blocking send) も同じ pump 上にあり、最大 4.4ms を実測

### 修正候補（非同期コピー）の A/B

`CEF_UNITY_ASYNC_COPY=1`。blit を encode + commit して即 return し、Mach 送信と shm 書き込みを
completion handler（直列 dispatch queue）で行う。in-flight 上限 2 で backpressure。

| 指標 | sync | async |
|---|---|---|
| pump_ticks（中央値） | 210〜333 | **407〜832** |
| copy_wait_total/s（中央値） | 515〜571ms | **26〜55ms** |
| copy_wait_max | 91〜356ms | 29〜46ms |
| received/s（中央値） | 45〜49 | 47〜51（有意差なし） |
| dropped | 0 | 1〜2/s |

順序を入れ替えても pump_ticks の優劣は一貫する。received/s に差が出ないのは、フレーム供給量そのものは
GPU/CPU 競合が律速だから。この修正が直すのは **pump の健全性**である。

### 正しさの検証

client 側に **GPU 読み**の検出器を追加した（自前 command queue で 1 列を staging buffer へ blit）。
CPU 読み (`IOSurfaceLock`) は lock 自体が GPU 同期を行うため識別力が無く、既知不良構成でも検出ゼロだった。
そのため **CPU 読みのサンプラは削除した**（残しても偽の安心を与えるだけなので）。検出器が生きていることは
`distinct_steps`（観測できた色 step の種類数、ページは 0..255 を循環する）で確認する。

| モード | gpu_verified | gpu_rollback |
|---|---|---|
| unsafe-no-wait（既知不良） | 2552（4 回分） | **6**（毎回 1〜2 検出） |
| sync（現行） | 2687 | 0 |
| async（修正案） | 1525 | 0 |

検出器は既知不良で 4/4 回反応し、async は sync と区別できない（既知不良のロールバック率 0.24% に対し
async 1525 フレームで 0 なので p≈0.025）。

**未検証**: ティアリング (`gpu_torn`) は既知不良でも一度も観測できず、この腕は未検証。観測できた破れは
常にロールバック（フレーム全体が古い）だった。async 固有の残存リスク「CEF がコールバック復帰後に src を
プールへ戻し、GPU がまだ読んでいる」はティアリングとして現れるはずなので、上記では除外できていない。

## #8 — shutdown が 500ms 以内に終了しない server を回収しない

競合下で `lifecycle 5` を実行し、各サイクルの `CefRuntime.Shutdown()` 直後に残存 server を数えた。

```
cycle=1 server_processes=0
cycle=2 server_processes=1   ← Shutdown 復帰後も存命
cycle=3 server_processes=0
cycle=4 server_processes=1   ← 同上
cycle=5 server_processes=0
```

競合なしでは 5/5 サイクルとも 0。**負荷時のみ発症**する。最終的には終了するので恒久ゾンビではなく
「Shutdown が終了を待たずに返る」形。Unity では親（Editor）が生き続けるため、存命した server が
継承済み FD（#9）を保持し続ける複合影響になる。

## #9 — server が親プロセスの FD を継承する

`.NET` のソケットは既定で `FD_CLOEXEC` が付くため、harness の素のソケットは継承されない
（server 側の lsof に現れない）。**Unity (mono) 側のソケットには付いていない**という前提を再現するため、
`fcntl(F_SETFD)` で CLOEXEC を明示的に外して spawn した。

```
parent_pid=42414 listening on 127.0.0.1:11565 fd=47 cloexec_cleared=True
cef-unity 42416 juha 47u IPv4 ... TCP localhost:11565 (LISTEN)
```

**同一 fd 番号 47 のまま server 側に出現**。spawn 側で不要 FD を閉じていないことが確定した。
再現には「親が CLOEXEC 無しの FD を持っている」ことが必要なので、harness 単体では
`cloexec_cleared` を明示しない限り発症しない。

## #10 — Mach receive port と native キャッシュが解放されない

`lifecycle` で Initialize/Shutdown を繰り返し、client プロセスの Mach port 総数を計測した。

```
mach_ports_before_any_init=39
cycle=1 mach_ports=73 receive_port= 9747 surface_cache=1
cycle=2 mach_ports=75 receive_port=18695 surface_cache=2
cycle=3 mach_ports=76 receive_port=20275 surface_cache=3
cycle=4 mach_ports=77 receive_port=19995 surface_cache=4
cycle=5 mach_ports=78 receive_port=17227 surface_cache=4
```

- **+1〜2 port/サイクルで単調増加**（初回のみ CEF 初期化分で +34）
- `receive_port` はサイクル毎に別の値になり、**古いポートを解放していない**
- `surface_cache` は前セッションの IOSurface を保持したまま上限 4 まで積み上がる
  （1920x1080 なら約 8MB × 4）

外部ツール不要で観測できるよう `cef_unity_debug_mach_port_count` / `cef_unity_debug_iosurface_state`
を追加した。

## #11 — メインスレッドの paint 待ち busy-wait

Unity 側 `ReceiveBeforeRender` を harness へ忠実移植（判定は同一の `CefZeroFramePacer`）。

| 条件 | wait_entered/s | spin（対実時間） | client CPU | 0F 達成率 | received/s |
|---|---|---|---|---|---|
| rAF 毎フレーム・競合なし・ON | **0** | 0% | 33ms/s | 0% | 60.0 |
| rAF・GPU+CPU 競合・ON | 1〜28 | 5〜22% | 20〜139ms/s | **0.9%** | 46〜50 |
| rAF・競合・OFF | 0 | 0% | 15〜37ms/s | 0% | 46〜50 |
| **5Hz 間欠・競合なし・ON** | **60（全フレーム）** | **42.5%** | **423ms/s** | **69%** | 5.2 |
| 5Hz 間欠・競合なし・OFF | 0 | 0% | 9〜25ms/s | 0% | 5.2 |

- 健常な連続アニメーション中は抑止パスが常に効き、**spin はゼロ**
- 発動するのは paint を取り逃し始めた時だけで、**その状態では 0F 達成 0.9%**（効かない時に限って発動する）
- 一番重いのは間欠ページで、60 フレーム中 55 フレームは damage が無く
  `NoDamageGiveUpMilliseconds`（7ms）まで空回りする → `block_avg_entered` が 7.0ms に張り付く
- received/s・ギャップは ON/OFF で差がない（順序を入れ替えると大小が逆転する）
- `block_avg` の分母バグ: old 0.12〜3.73ms vs entered 6.60〜9.41ms（**3〜15 倍の過小評価**）

**含意**: 42% の spin のほぼ全部は「damage が無かったことを 7ms かけて発見する」空回りである。
サーバーが「このフレームは damage なし」を shm に 1 ビット書けば即座に抜けられるので、
#11 は「廃止」ではなく「サーバーからの damage 通知 + opt-in」に落とせる。

## #12 — 1000Hz 固定 message pump

`CEF_UNITY_PUMP_INTERVAL_MILLISECONDS` で fallback 間隔を変えて比較（CEF 要求駆動 `schedule_pump` は常に併存）。

| 間隔 | pump_ticks/s | paints/s | server CPU | BeginFrame→paint |
|---|---|---|---|---|
| 1ms（現行） | 977 | 60 | 120ms/s | average 3.67ms / median 3.67ms |
| 4ms | 352 | 60 | 100ms/s | 3.42 / 3.77 |
| 8ms | 280 | 60 | 90ms/s | 3.86 / 3.89 |
| 16ms | 202 | 60 | 85ms/s | 3.56 / 3.84 |

16ms 指定でも 202 ticks/s 回るのは `schedule_pump`（CEF 要求駆動）が主に効いているため。

0F 達成率（5Hz 間欠ページ、30 秒 n≈137）: 間隔 1ms で 56.9%、8ms で 66.2% と**逆転**する。
短時間（8 秒 n=25）では 68.0% → 56.0% と逆向きに出たので、**差はノイズ**と判断できる。

**結論**: 固定 1000Hz fallback は paint 数・レイテンシ・0F 達成率のいずれにも測定可能な寄与がなく、
server CPU を約 30%（120→85ms/s）余分に使っている。issue に書いた「単独で変更すると 0F 化の効果が
失われる」という懸念は、この harness では確認できなかった。

## #13 — BeginFrame の burst drain

1 tick で drain したコマンド数と、そのうち BeginFrame の数の最大を計測した。

- 競合なし: `drain_max=1〜3` / `begin_frame_burst_max=1〜3`
- GPU + CPU 競合（pump 261〜579 ticks/s）: `drain_max=1〜4` / `begin_frame_burst_max=1〜4`

**バーストは小さい**。pump が 60Hz よりずっと速く回っている限り、蓄積は 1〜2 件で終わる。
バースト規模は pump 停止時間 × 60Hz なので、#7 の 356ms ストールが起きた窓では約 21 件になる計算だが、
今回の 12 窓ではそこまでの停止が発生しなかった。**#13 の深刻度は #7 の下流**であり、
#7 を直せば自然に縮む。単独での優先度は低い。

## #14 — dirty rect 未使用

`on_accelerated_paint` が受け取る dirty rect の面積合計と転送面積（= viewport 全面）の比を計測した。

| ページ | damage_ratio |
|---|---|
| 進捗バー相当（小 damage） | **0.0〜3.3%** |
| rAF 全面更新 | 100.0% |

小 damage ページでは大半の窓が 0.0%（1px×8px = 全体の 0.0004% が四捨五入で 0.0 になる）。
moorestech レポートの「dirty rect 面積は 2〜4%」は再現し、実際にはさらに小さい場合もある。
コピーは常に全面なので、**帯域の 96〜100% が無駄**という主張は妥当。

ただし転送先は `POOL_SIZE 5` のラウンドロビンなので、差分だけ blit すると 5 フレーム前の内容が
残る領域が壊れる。サーフェス毎の damage 累積かプール構成の変更が前提になる（issue 記載どおり）。

## #15 — VERBOSE ログ常時出力

`--logging` OFF（`Initialize(enableLog: false)`）で `smoke` を実行:

- ラッパ自前ログ `cef_unity_server.log`: **生成されない**（期待どおり）
- CEF 本体ログ `cef_debug.log`: **749KB**（約 10 秒の実行で）

`settings.log_file` / `log_severity = VERBOSE` が `LOG_ENABLED` と無関係に設定されていることを確認した。

## 残作業（優先順、2026-07-30 時点）

1. **#7 を終わらせる（残り 1 手）**: ティアリング検証だけがブロッカー。`CEF_UNITY_SLOW_COPY=n` 相当の
   デバッグモードで blit を重ねて GPU 読み窓を広げ、「CEF がコールバック復帰後に src をプールへ戻し、
   GPU がまだ読んでいる」リスクが実在するか判定する。白なら既定を async に切替 → `deploy.sh` →
   main マージ。採用理由は fps ではなく「入力・JS タイマー・IPC が最大 356ms 凍結するのを止める」こと
2. **#10**: `mach_iosurface_client_disconnect()` を作り `cef_unity_shutdown` から呼ぶ。描画に触らず
   リスクほぼゼロ。検証は `lifecycle` で `mach_ports` が横ばいになるかを見るだけ
3. **software 経路の seqlock 欠陥（未起票）**: `read_frame_does_not_mix_frames_while_writer_advances`
   が 5 回中 3 回失敗する（失敗は 0.01 秒で即決、成功は 0.5 秒）。`8d629af` のリトライ 1 回固定では
   性質を保証できていない。Linux の唯一の本番経路であり、かつ CLAUDE.md が必須手順にしている
   `cargo test -p cef-unity-ipc` が常時赤で機能していない
4. **#15**: `settings.log_file` / `log_severity` を logging フラグで分岐（実質 3 行）
5. **#8**: 500ms 固定 sleep → 期限付き待ち + `kill` + `Drop`。検証は `lifecycle` で 5/5 サイクル 0
6. **#12**: one-shot timer 化。回帰確認（paints 60/s・レイテンシ・0F 達成率が不変、CPU が下がる）は
   harness で自動化済み
7. **#11**: まずカウンタ分離 + 既定 `_zeroFrameWaitMilliseconds = 0`（opt-in 化）。手調整の塊なので
   一気にやらない。0F を取り戻すなら後から「サーバー側の damage なし通知」を足す
8. **#9**: 先に Unity 実機で `lsof -p <server_pid> | grep <ゲームサーバーのポート>` を 1 回。
   **測る前に直さない**
9. **#14**: 棚上げ（P3 相当）
10. **#13**: 実測で前提が崩れたためクローズ済み（#7 の下流）

## この調査で削除したもの

- **CPU 読みの画素サンプラ** (`cef_unity_sample_iosurface_pixels`): 既知不良構成でも検出ゼロで
  識別力が無いことを実証したため削除。GPU 読み版のみ残す
- **legacy IOSurfaceLookup 経路** (`cef_unity_create_metal_texture`): macOS 11 で deprecated・
  macOS 16 でプロセス間無効化されており、リポジトリ内に呼び出し元が 1 つも無かった
- **非 Windows 用の未使用スタブ** `pub type DXGI_FORMAT = u32` (`d3d11_pool.rs`): ビルド警告 2 件の原因

## 計測上の注意

- harness は BF#1 と recv の間にゲーム処理・描画が入らないため、0F 待ちの spin は**最悪ケース**を測る。
  実 Unity では重いフレームほど窓が食われて spin は減る
- `received/s` は「fresh フレームを取得できたポーリング回数」で、サーバーの paint 数とは別物
  （サーバー側の `paints` は STATISTICS 参照）
- 計装は `--logging` 有効時のみ動作する（無効時は `Instant::now()` すら呼ばない）
- 1 回の実行ごとに `$TMPDIR/cef_unity_server.log` は作り直される（前回分は残らない）
