# Unity 抜き harness による issue #7〜#15 の再現・計測

- 計測日: 2026-07-30（#7 の修正と再計測は 2026-07-31）
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

server 側のトグル（すべて環境変数）:

| 変数 | 効果 |
|---|---|
| `CEF_UNITY_SYNC_COPY=1` | GPU コピーを同期版（issue #7 の修正前）に戻す。既定は非同期 |
| `CEF_UNITY_NO_BEGIN_FRAME_GATE=1` | BeginFrame ゲートを外す（**既知不良**。poison 検出器の positive control 専用） |
| `CEF_UNITY_SLOW_COPY=n` | blit をバンド分割して n 回のダミー全面 blit を挟み、転送元の読み出し時間を広げる（検証専用） |
| `CEF_UNITY_UNSAFE_NO_WAIT=1` | 完了を待たず即送信（**既知不良**。転送先側の検出器の negative control 専用） |
| `CEF_UNITY_PUMP_INTERVAL_MILLISECONDS=n` | pump fallback 間隔（既定 1、#12 の A/B 用） |

## 結果一覧

| issue | 再現 | 実測 |
|---|---|---|
| #7 P0 同期 GPU コピーが pump を止める | ✅ **修正済** | copy_wait が実時間の 50〜67%、単発最大 **796ms**。非同期化 + BeginFrame ゲートで解消 |
| #8 P0 shutdown が server を回収しない | ✅ | 競合下で **5 サイクル中 2 回**、Shutdown 復帰後も server が存命 |
| #9 P1 server が親の FD を継承する | ✅ (条件付き) | CLOEXEC を外した LISTEN ソケットが同一 fd 番号で server に出現 |
| #10 P1 Mach port / キャッシュのリーク | ✅ | Mach port が **+1〜2/サイクル**で単調増加、surface キャッシュは上限 4 まで蓄積 |
| #11 P1 メインスレッドの busy-wait | ✅ **修正済** | 既定 OFF 化で spin 0%、opt-in 時のみ従来動作 (間欠ページで spin **42.4%**・CPU 約 25 倍) |
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
| GPU + CPU 飽和（強め・追試） | 7〜295 | 291〜768ms | **796ms** | 1〜22 |

- **競合が無くても実時間の 4〜15% は `waitUntilCompleted` で寝ている**
- 単発 300ms 超は「GPU 競合だけ」では起きず、**待機スレッドの CPU 追い出しが加わって初めて**再現する
- Mach 送信 (`mach_msg`、10ms timeout の blocking send) も同じ pump 上にあり、最大 4.4ms を実測

### CEF の転送元契約 — 「非同期化するだけ」では不正

`cef_render_handler_capi.h` の `on_accelerated_paint` に明文の契約がある:

> The handle's resource cannot be cached and cannot be accessed outside of this callback. …
> **The contents of |info| will be released back to the pool after this callback returns.**

つまり「blit を投げて即 return する」だけの非同期化は、GPU がコールバック復帰後も転送元を
読み続けるので契約違反になる。当初の実装（in-flight 上限 2 で backpressure するだけ）は
これに該当していた。ティアリングが観測されなかったのは運が良かっただけで、安全性の根拠にならない。

前提の実測も 2 つ外れていた:

- **転送元プールは 2 枚固定ではない**。競合なし 1280x720 では `id=315, 813` の厳密な交互だが、
  GPU 競合下 1920x1080 では十数個の id が出る。「1 枚だけ先行させてよい」とは言えない
- **CEF は同じ surface に対して毎回異なる `IOSurfaceRef` ポインタを渡す**
  （`id=315` に対し `0x11409cffe40` / `0x11409abbd00` / `0x1140a2316c0`）。
  ポインタで同一性を判定するコードは常に不一致になる。`g_src_cache` はこれで毎フレーム
  cache miss して `MTLTexture` を作り直していた（キーを `IOSurfaceID` に修正済み）

### 修正: 非同期コピー + BeginFrame ゲート

1. blit を encode + commit して即 return し、Mach 送信と shm 書き込みを completion handler
   （直列 dispatch queue）で行う。**pump スレッド上に GPU 待ちが一切無くなる**
2. 転送元を読んでいる blit がある間は **BeginFrame を発行しない**（`begin_frame_gate_open`）。
   CEF は BeginFrame でしか描かず、BeginFrame は pump 上でしか処理されないので、これで
   「読み取り中の転送元に CEF が描き込む」ことが構造的に起きない。**待つのではなく発行しない**
   のが要点で、pump は回り続ける = 入力・JS タイマー・IPC は止まらない。
   見送った BF#1 は tick でゲートが開き次第発行する（`begin_frame_deferred` に計上）
3. 防御として、CEF が読み取り中の転送元を再交付したらその blit の結果を捨てる
   （poison、`poisoned_total` に計上）。ゲートが効いていれば 0 のまま

直列化してもフレーム供給レートは同期版と変わらない（同期版も 1 コピー完了ごとに 1 フレーム）。

#### A/B（GPU 競合 2 本 + CPU 飽和 4 本、順序を入れ替えて各 2 回、1920x1080）

| 指標 | sync | async（修正後） |
|---|---|---|
| copy_wait_total/s（中央値） | 464 / 537ms | **9.4 / 10.0ms** |
| copy_wait_max（最大） | 66 / 202ms | 17 / 94ms |
| received/s（中央値） | 54 / 38 | 54 / 32 |
| poisoned | 0 | **0** |

競合なし（1920x1080）では sync の `copy_wait_total` 74〜113ms/s・`copy_wait_max` 2.9〜5.3ms に対し、
async は 1.9〜8.5ms/s・0.08〜3.8ms で、**paints は両者とも 60/s**（ゲートによるフレームレート低下は無い）。

`copy_wait_total` の 50 倍差は順序を入れ替えても一貫する。`pump_ticks` と `received/s` は
実行順（マシン負荷の漂流）の影響が支配的で、A/B の判定には使えない。
**この修正が直すのは pump の健全性**であり、フレームレートそのものではない。

### 正しさの検証

**転送先側**（client が blit 未完了の surface を読む）: client 側に GPU 読みの検出器がある
（自前 command queue で 1 列を staging buffer へ blit）。CPU 読み (`IOSurfaceLock`) は lock 自体が
GPU 同期を行うため識別力が無く、既知不良構成でも検出ゼロだったので **CPU 読みのサンプラは削除した**。

| モード | gpu_verified | gpu_rollback |
|---|---|---|
| unsafe-no-wait（既知不良） | 2552（4 回分） | **6**（毎回 1〜2 検出） |
| sync | 2687 | 0 |
| async（修正後、競合下 4 回 + 低速コピー下 4 回） | 3400+ | 0 |

**転送元側**（CEF が読み取り中の転送元へ描き込む）: `CEF_UNITY_SLOW_COPY=64` で転送元の
読み出し時間を人為的に広げ（16 バンドに分割し、バンド間に全面ダミー blit を 64 回挟む）、
ゲートの有無で比較した。

| 構成 | poisoned（20 秒 × 2 回） |
|---|---|
| ゲート **OFF** + 低速コピー（既知不良） | **2, 3**（2/2 回で発火） |
| ゲート **ON** + 低速コピー | **0, 0** |

検出器が生きていること（ゲート OFF で 2/2 発火）と、ゲートが実際に上書きを防いでいること
（ON で 0）の両方を確認した。既知不良の発生率 ~2.5 件/18 秒に対しゲート ON は 38 秒で 0 件なので
p ≈ 0.007。**前回「未検証」としていたティアリングのリスクは、観測を待つ代わりに構造的に潰す形で解決した**。

さらにゲート ON では `distinct_steps == gpu_verified`（80/80, 72/72 = 受信フレームの色 step が
すべて異なる）だが、ゲート OFF では 64/87, 59/82 と重複が出る。

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
| **5Hz 間欠・競合なし・ON** | **60（全フレーム）** | **42.5%** | **423ms/s** | **69%※** | 5.2 |
| 5Hz 間欠・競合なし・OFF | 0 | 0% | 9〜25ms/s | 0% | 5.2 |

※単一サンプル（1 回のみの計測）。後述「修正後」節の A/B 検証で 20.9〜56.3%（平均 33.4〜42.9%）まで
ばらつくことが判明しており、単一の 69% ではなくこの分布として扱うべきである。

- 健常な連続アニメーション中は抑止パスが常に効き、**spin はゼロ**
- 発動するのは paint を取り逃し始めた時だけで、**その状態では 0F 達成 0.9%**（効かない時に限って発動する）
- 一番重いのは間欠ページで、60 フレーム中 55 フレームは damage が無く
  `NoDamageGiveUpMilliseconds`（7ms）まで空回りする → `block_avg_entered` が 7.0ms に張り付く
- received/s・ギャップは ON/OFF で差がない（順序を入れ替えると大小が逆転する）
- `block_avg` の分母バグ: old 0.12〜3.73ms vs entered 6.60〜9.41ms（**3〜15 倍の過小評価**）

**含意**: 42% の spin のほぼ全部は「damage が無かったことを 7ms かけて発見する」空回りである。
サーバーが「このフレームは damage なし」を shm に 1 ビット書けば即座に抜けられるので、
#11 は「廃止」ではなく「サーバーからの damage 通知 + opt-in」に落とせる。

### 修正後 (2026-07-31)

修正内容: Unity `_zeroFrameWaitMilliseconds` の既定を `10f`→`0f` にして opt-in 化、開発トグルを
`cef_no_zero_wait`→`cef_zero_wait` に反転、計装カウンタを `CefZeroFrameWaitStatistics`
(`CefUnity.Core/Scroll/`) として恒久化し `block_avg` の分母を「実際に待機へ入った回数
(`wait_entered`)」に統一した。

**既知の制限（serialized 値による既定値の無効化）**: この既定値の変更は新規に追加した
コンポーネントにのみ効く。フィールドには `[FormerlySerializedAs("_zeroFrameWaitMs")]` が付いて
おり、既存の scene/prefab に `_zeroFrameWaitMs`（または `_zeroFrameWaitMilliseconds`）が
保存されている場合は serialized 値が優先されるため、初期化子の `0f` は上書きされる。
アップグレード時は Inspector で明示的に 0 へ変更する必要がある。既知の該当例:
moorestech `MainGameUI.prefab` に `_zeroFrameWaitMs: 10` が保存済み
（`docs/MOORESTECH_PR1097_VERIFICATION.md:42`）。つまり主要な下流消費者ではこの opt-in 化の
効果がそのままでは 0 になる。

`CefUnity.Harness zero-frame-wait` で Unity と同じ `CefZeroFramePacer` /
`CefZeroFrameWaitStatistics` を使い、既定 OFF と opt-in ON を再計測した
(load average 2.1〜6.5、5Hz 間欠ページ、各 20 秒 = 有効窓 17 秒):

| 条件 | wait_entered/s | spin_share | block_avg (wait_entered 分母) | 0F 達成率 | received/s |
|---|---|---|---|---|---|
| 既定 OFF (`zeroFrameWaitMilliseconds=0`) | **0.0** | **0.0%** | 0.00ms | 0.0% | 5.1 |
| opt-in ON (`=10`、1 回目) | 60.6 | 42.4% | 6.99ms | 36.0% | 5.1 |
| opt-in ON (`=10`、2 回目) | 60.6 | 42.4% | 7.00ms | 40.7% | 5.1 |
| opt-in ON (`=10`、3 回目) | 60.8 | 42.5% | 7.00ms | 56.3% | 5.1 |
| opt-in ON (`=10`、4 回目) | 60.7 | 42.5% | 6.99ms | 38.4% | 5.1 |

- **既定 OFF**: `wait_entered_per_second=0.0` / `spin_share=0.0%`、各窓の `no_wait` がほぼ全数
  (60〜61/61) になり、busy-wait が既定で消えたことを実行時証拠として確認した
- **opt-in ON**: `wait_entered_per_second`（60.6〜60.8。ブリーフの期待レンジ 55〜60 をわずかに
  超えるが誤差の範囲とみている）/ `spin_share`（42.4〜42.5%）/ `block_avg`（6.99〜7.00ms）は
  4 回とも安定しており、旧実測 (wait_entered 60・spin 42.5%・cpu 423ms/s) ともほぼ一致する。
  opt-in 経路そのものは壊れていない
- **`zero_frame_share`（0F 達成率）は 4 回で 36.0% / 40.7% / 56.3% / 38.4%（平均 42.9%）と
  試行ごとに大きくばらつき、旧基準の 69% には毎回届かなかった。**
- **load average の但し書き**: 実装後 4 回目 (`zero_frame_share=38.4%`) は計測直前の `uptime` が
  load average 5.62 で、計測手順の上限（5 未満）を超過していた。ただし `wait_entered`・
  `spin_share`・`block_avg` は他 3 回と一致していたため、この試行を除外していない
- **paint 供給の回帰確認**（`paint-statistics 20 1920 1080 animation`）: `received` 中央値
  61/s（`received_min=58` を除き概ね 60〜61）、`gpu_verified=1306` に対し `gpu_torn=0` /
  `gpu_rollback=0` で回帰なし。`BeginFrame→paint latency` はサーバーログ実測で
  平均 4.36ms（n=21 窓の平均、期待 3〜4ms よりわずかに高いが #12 記載の 3.67ms と同水準）
- **ライフサイクルの回帰確認**（`lifecycle 5`）: 5/5 サイクル完走、`mach_ports` は
  70→72→73→74→75（+2, +1, +1, +1）で #10 の既知水準（+1〜2/サイクル）から悪化なし、
  `server_processes_final=0`

**戻し方と代償**: 0F 待ちが必要な場面では以下のいずれかで opt-in できる。
①Inspector で `_zeroFrameWaitMilliseconds` に正の値（旧既定は `10`）を入れる。
②開発ビルド限定 (`#if CEF_UNITY_DEV_TOOLS && (UNITY_EDITOR || DEVELOPMENT_BUILD)` 配下、
リリースビルドでは効かない) で `$TMPDIR/cef_zero_wait` にマーカーファイルを置く。
③戻さない場合（既定 OFF のまま）の代償: 5Hz 級の間欠更新ページで最大 16.7ms（1 フレーム分）の
追加遅延が生じる。ただし `received/s` とフレーム間ギャップは ON/OFF で不変なので、コマ落ちや
供給量そのものの劣化は起きない。

**server-side flush の位置づけメモ**: 既定 OFF ではクライアントが flush 結果の到着を待たなく
なるため、server-side flush（server.rs の BF#1 +3/+6ms 内部 flush）由来の paint は同フレームの
present には乗らず、次フレームの受信で拾われる。つまり「0F 化」自体の効果は既定では消え、
残るのは「次フレームの内容が数 ms 新しい」という効果だけになる（実測では `received/s` が
ON/OFF で不変であり、コストが増えた証拠もない。`paints/s` は `paint-statistics` の単独実行でしか
計測しておらず、0F 待ちの ON/OFF を振った比較はまだ取っていない）。残作業 7 番の「次の一手 = サーバーからの
damage なし通知」と併せて、server-side flush の存在意義そのものも opt-in 化後の構成でどこまで
必要かを再評価する必要がある。

#### A/B: 実装前 (commit `0ed5391`) の harness との比較（2026-07-31 追加検証）

`zero_frame_share` の低下が「今回の Task 1〜3 のリファクタが待機ロジックを壊した」せいか、
「測定対象が変わっただけ」なのかを切り分けるため、リファクタ前の
`ZeroFrameWaitCommand.cs`（commit `0ed5391`、`CefZeroFramePacer` や `Receive()` の判定ロジックは
現行と同一で、集計先が独自フィールドか `CefZeroFrameWaitStatistics` かだけが違う）を一時的に
チェックアウトしてビルドし、同一条件・同一手順（`pkill` → `uptime` →
`zero-frame-wait 20 10 1920 1080 intermittent`）で 4 回計測した:

```bash
git checkout 0ed5391 -- cef-unity-csharp/CefUnity.Harness/ZeroFrameWaitCommand.cs
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug
# 計測後
git checkout HEAD -- cef-unity-csharp/CefUnity.Harness/ZeroFrameWaitCommand.cs
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug
```

| 版 | 試行1 | 試行2 | 試行3 | 試行4 | 平均 | 範囲 |
|---|---|---|---|---|---|---|
| 実装前 (`0ed5391`) | 52.3% | 29.1% | 20.9% | 31.4% | **33.4%** | 20.9〜52.3% |
| 実装後 (現行 HEAD) | 36.0% | 40.7% | 56.3% | 38.4% | **42.9%** | 36.0〜56.3% |

**判定（暫定、n=4+4）: 今回の Task 1〜3 のリファクタが `zero_frame_share` を悪化させたという
証拠は見られなかった。** 実装前の harness でも `zero_frame_share` は 20.9〜52.3%（平均 33.4%）
まで大きくばらつき、69% には一度も届かなかった。実装後（平均 42.9%）より低い試行すらあり、
両者の分布は明確に重なる（実装前 4 回中 3 回が実装後の最小値 36.0% を下回る）。ただし試行数は
各 4 回、レンジは平均の半分近くに達しており、有意差検定は行っていない。#12 の 0F 達成率比較
（n≈137、「差はノイズ」）と比べても本件ははるかに小標本であり、「無罪の確定」と言い切れる
強さの根拠ではない点に注意。

`wait_entered_per_second`（60.6〜60.8）・`spin_share`（42.4〜42.6%）は両版とも `CLIENT_SUMMARY`
で安定していた。`block_avg` は実装後の `CLIENT_SUMMARY`（`block_avg_entered`）では 6.99〜7.00ms、
実装前は `CLIENT_SUMMARY` に集計値が無いため 1 秒窓ごとの `block_avg_entered` を見ると
6.96〜7.10ms で、いずれも同水準だった。一方 `zero_frame_share` だけが両版で試行間 20〜31
ポイントも揺れている（実装前レンジ 31.4pt、実装後レンジ 20.3pt）。5Hz 間欠ページの
`setInterval(200ms)` と 60Hz の `SendExternalBeginFrame` は独立クロックなので、**仮説として**
この揺れはプロセス起動ごとの位相関係（スケジューリングジッタ）に由来する測定対象そのものの
高分散ではないかと考えている（位相そのものを直接計測して裏付けたわけではない）。

したがって、旧基準の 69% は #7 修正の影響というより、そもそも 1 回しか採取していない
サンプルが高分散な分布のたまたま高い側に当たっていた可能性がある、というのが現時点での
見立てである。#11 の opt-in を語る際は「単一の 69%」ではなく、今回得られた 20.9〜56.3%
（8 試行全体、実装前後合算）を分布として扱うべきである。

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

1. ~~**#7**~~ **完了**（2026-07-31）: 非同期コピー + BeginFrame ゲートで既定を切り替えた。
   転送元契約の破れは `CEF_UNITY_SLOW_COPY` を使った positive/negative control で検証済み
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
7. ~~**#11**~~ **完了**（2026-07-31）: カウンタ分離 (`CefZeroFrameWaitStatistics`) + 既定
   `_zeroFrameWaitMilliseconds = 0`（opt-in 化、開発トグル `cef_zero_wait`）で spin を既定 0 にした。
   0F を取り戻すなら次の一手として「サーバー側の damage なし通知」（#11 セクション参照）が
   残っている。未着手
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

## v0.6.0 リリース前の回帰計測（2026-07-31）

対象は `8f6770d`（v0.6.0 タグ対象）。このリリースで追加した 3 件の修正はいずれも macOS の実行
経路を変えない（①非 macOS 向け `use_unsafe_no_wait_copy` スタブ、②Windows 専用の
`GetProcessTimes` 経路、③実運用で呼ばれていない `SharedMemoryReader::read_frame` の
リトライ挙動）。回帰が無いことの確認として 3 コマンドを 1 回ずつ実行した。

⚠️ **環境の但し書き**: 計測直前の load average は **7.59** で、計測手順の上限（5 未満）を
超過している（直前に `cargo build` を回したため）。絶対値の比較には向かない条件だが、
下記のとおり既知水準から外れた指標は無かった。

| コマンド | 実測 |
|---|---|
| `paint-statistics 20 1920 1080 animation` | `mode=async`、`paints=60/s`（初回窓のみ 67、末尾に 102 の窓が 1 つ）、`copies=paints` で `dropped=0`、`poisoned_total=0`、`copy_wait_max` 0.14〜0.90ms（初回窓のみ 6.34ms）、`pump_ticks` 約 1020〜1080/s |
| `zero-frame-wait 20 10 1920 1080 intermittent` | `zero_frame_share=34.9%`、`received/s=5.1`、`wait_entered/s=60.7`、`spin_share=42.6%`、`block_avg=7.02ms`、`delay_2F+=0` |
| `lifecycle 5` | 5/5 サイクル完走、`mach_ports` 70→72→73→74→75、`server_processes_final=1` |

**判定: 回帰は見られない。**

- `paints=60/s` で `dropped=0` / `poisoned_total=0` は #7 修正後の水準どおり。BeginFrame ゲートが
  効いており転送元の契約違反はゼロ
- `mach_ports` の推移（70→72→73→74→75）は前回計測と**完全に一致**し、#10 の既知水準
  （+1〜2/サイクル）から悪化なし
- `server_processes_final=1` は前回計測の 0 と異なるが、これは #8 の「**負荷時のみ発症**する
  （Shutdown が終了を待たずに返る）」既知挙動で、load 7.59 の高負荷下という今回の条件と整合する
- `zero_frame_share=34.9%` は実装後 4 回の範囲（36.0〜56.3%）をわずかに下回るが、この指標は
  A/B 検証のとおり実装前後とも 20.9〜56.3% と大きくばらつくもので、`wait_entered/s`・
  `spin_share`・`block_avg` は過去の計測と一致している

## 計測上の注意

- harness は BF#1 と recv の間にゲーム処理・描画が入らないため、0F 待ちの spin は**最悪ケース**を測る。
  実 Unity では重いフレームほど窓が食われて spin は減る
- `received/s` は「fresh フレームを取得できたポーリング回数」で、サーバーの paint 数とは別物
  （サーバー側の `paints` は STATISTICS 参照）
- 計装は `--logging` 有効時のみ動作する（無効時は `Instant::now()` すら呼ばない）
- 1 回の実行ごとに `$TMPDIR/cef_unity_server.log` は作り直される（前回分は残らない）
