# 0F 待ち busy-wait の opt-in 化と計装カウンタ分離 (issue #11)

## 背景

Unity メインスレッドは `CefUnityBrowserSample.ReceiveBeforeRender` で、server-side flush の結果
(`accelerated_frame_id` の増分) が届くのを `Thread.SpinWait(64)` ループで待つ。既定の待ち上限は
`_zeroFrameWaitMilliseconds = 10f`。この待ちは「同フレームの present に最新 paint を乗せる」
(0F 化) ためのもの。

Unity 抜き harness (`CefUnity.Harness zero-frame-wait`) による実測 (`docs/HARNESS_MEASUREMENTS.md`
の #11) で、費用対効果が条件によって破綻していることが分かった。

| 条件 | wait_entered/s | spin (対実時間) | client CPU | 0F 達成率 |
|---|---|---|---|---|
| rAF 毎フレーム・競合なし | 0 | 0% | 33ms/s | 0% |
| rAF・GPU+CPU 競合 | 1〜28 | 5〜22% | 20〜139ms/s | **0.9%** |
| 5Hz 間欠・競合なし | 60 (全フレーム) | **42.5%** | 423ms/s | 69% |

- 健常な連続アニメーション中は抑止パスが常に効いて **spin はゼロ**、0F 達成も 0% で、待ちは
  何も生んでいない
- 競合下では抑止推定が外れて spin が復活するが、その状態の 0F 達成は 0.9%。
  **効かない時に限って発動する**構造で、`paint 低下 → 抑止が外れる → spin が CPU を焼く →
  paint さらに低下` の正帰還になり得る
- 恩恵が出るのは 5Hz 間欠ページのみ (0F 69%) だが、その代償は実時間の 42.5% の spin と
  OFF 比 25 倍の CPU。**16.7ms の短縮を毎フレーム 7ms の spin で買っている**交換レート

加えて計装に 2 つの欠陥がある。

1. `_doublePumpFreshCount` / `_doublePumpFallbackCount` は**抑止スキップパス (ブロックしない)
   でも加算される**のに、`block_avg` の分母がこの合計になっている。実測で `block_avg_old`
   0.12〜3.73ms に対し実際の待機 1 回あたりは 6.60〜9.41ms — **3〜15 倍の過小評価**
2. 抑止スキップの回数を数えるカウンタが無く、待ちに入らなかった理由を切り分けられない

## ゴール

- 既定状態の Unity メインスレッドから busy-wait を消す
- 0F 待ちを試したい場合の経路は残す (定数群は実測チューニングの成果物であり、削除は不可逆)
- 計装が「実際に待機へ入った回数」を測れるようにし、同じ分母バグが再発しない構造にする

## 非ゴール

- `CefZeroFramePacer` の定数群 (`FreshPaintMinDelayMilliseconds` 等) と streak 推定ロジックの変更。
  手調整の塊であり、既定 OFF になれば実行されない
- サーバー側からの damage なし通知の実装 (issue の第 3 案)。0F を取り戻す必要が出た時点で別途
- 待機コード自体の削除

## 設計

### 1. 既定値を 0 にして opt-in 化

`cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs`:

- `_zeroFrameWaitMilliseconds` の初期化子を `10f` → `0f`
- Tooltip を更新する。「既定 0 = 無効 (常にノンブロッキング受信)。正の値を入れると 0F 化と
  引き換えにメインスレッドが最大この時間 spin する」旨を書く
- 開発トグルを反転する。現行の `cef_no_zero_wait` (存在すると 0 にする) を `cef_zero_wait`
  (存在すると 10ms にする) へ置き換える。既定が OFF になった以上、比較したい側が opt-in する

`SampleScene.unity` にはこのフィールドの serialized 値が保存されていない (確認済み) ため、
初期化子の変更がそのまま実効値になる。シーンファイルの編集は不要。

`_zeroFrameWaitMilliseconds <= 0f` のとき `ReceiveBeforeRender` は既存の早期 return で
ノンブロッキング受信のみを行う。この経路は `CefUnity.Viewer` の `CefFrameSource.TickFrame`
と同じ挙動 (新フレームが無ければ直前のテクスチャを維持) になる。

### 2. 計装カウンタを `CefUnity.Core` へ切り出す

Unity 側のカウンタは MonoBehaviour のフィールドに直書きで、加算位置の誤りをテストで
検出できない。これが分母バグを生んだ構造なので、集計を純 C# クラスへ移す。

新規: `cef-unity-csharp/CefUnity.Core/Scroll/CefZeroFrameWaitStatistics.cs`

保持する値:

| 名前 | 意味 |
|---|---|
| `FreshCount` | 新 paint を取得できた回数 |
| `FallbackCount` | 取得できず前フレーム内容を継続表示した回数 |
| `NoWaitCount` | 待ちが無効 (既定) でノンブロッキング受信のみ行った回数 |
| `IdleSkipCount` | プローブ判定 (静止中) で待ちをスキップした回数 |
| `SuppressedSkipCount` | damage streak 抑止推定・連続入力で待ちをスキップした回数 |
| `WaitEnteredCount` | **実際に spin ループへ入った回数** |
| `SpinTotalMilliseconds` | spin の合計時間 |
| `SpinMaximumMilliseconds` | spin の最大時間 |

公開する API:

- `RecordNoWaitReceive(bool receivedFreshPaint)` — 待ち無効時 (既定経路)
- `RecordIdleSkip(bool receivedFreshPaint)`
- `RecordSuppressedSkip(bool receivedFreshPaint)`
- `RecordWaitCompleted(bool receivedFreshPaint, double spinMilliseconds)` — `WaitEnteredCount`
  もここで加算する。待機に入ったのに記録し忘れる、あるいは抑止パスで加算される事故を
  防ぐため、待機回数と spin 時間は必ず同じ呼び出しで入る
- `BlockAverageMilliseconds` — 分母は `WaitEnteredCount` (0 のときは 0 を返す)
- `Reset()`
- `FormatLine()` — 1 秒窓のログ行を組み立てる

加算規則を 2 つの軸に分ける。これがカウンタの意味を一意にし、分母バグの再発を防ぐ:

- **受信の成否**: `FreshCount` / `FallbackCount` は 4 つの `Record*` すべてで加算する。
  合計はその窓で recv を試みたフレーム数になる (現行実装は idle パスで加算しないため
  合計がフレーム数と一致せず、これも分母を歪める要因だった)
- **通ったパス**: `NoWaitCount` / `IdleSkipCount` / `SuppressedSkipCount` / `WaitEnteredCount`
  はそれぞれ対応する 1 つの `Record*` でのみ加算する。合計は上と同じフレーム数になる

ログ行の書式 (`suppressed_skip` と `wait_entered` を追加)。このログは待ちが有効なときだけ
出力されるので、以下は opt-in で 10ms を指定し競合が起きている 1 秒窓の例:

```
[CefUnity] 0F-wait: fresh=48 fallback(1F)=12 idle=0 suppressed_skip=42 wait_entered=18 block_avg=7.10ms block_max=9.41ms
```

`suppressed_skip + wait_entered = 60` (idle と no_wait は 0) でフレーム数と一致する。
旧実装の `block_avg` は同じ窓で 60 を分母にするため 2.13ms と表示され、実際の 7.10ms を
3 倍以上過小評価していた。

`CefUnityBrowserSample` は自前のカウンタ 5 本を捨ててこのクラスへ委譲する。
`CefUnity.Harness` の `ZeroFrameWaitCommand.WindowCounters` のうち共通部分 (上表の 7 項目) も
このクラスへ置き換える。harness 固有の項目 (`ReceivedCount` / `MaximumGapMilliseconds` /
0F 遅延分布) は `WindowCounters` 側に残す。

### 3. テスト

新規: `cef-unity-csharp/CefUnity.Tests/CefZeroFrameWaitStatisticsTests.cs`

- 抑止スキップ・idle スキップ・待ち無効の受信を何回記録しても `WaitEnteredCount` が
  0 のままであること
- `RecordWaitCompleted` を n 回呼ぶと `WaitEnteredCount == n` になり、
  `BlockAverageMilliseconds` が spin 合計 ÷ n になること
- 抑止スキップと待機完了が混在する窓で、平均が待機回数だけを分母にすること
  (= 旧実装の分母バグの回帰テスト)
- 4 パスを混在させたとき、`FreshCount + FallbackCount` と
  `NoWaitCount + IdleSkipCount + SuppressedSkipCount + WaitEnteredCount` が一致すること
- `SpinMaximumMilliseconds` が最大値を保持すること
- `Reset()` で全カウンタが 0 に戻ること
- `FormatLine()` が `wait_entered=` と `suppressed_skip=` を含むこと

`CefZeroFramePacer` は無変更なので既存の `CefZeroFramePacerTests` はそのまま通る。

### 4. 検証

- `dotnet test` (`cef-unity-csharp/CefUnity.slnx`) が全緑であること
- `bash cef-unity-csharp/build-csharp.sh` で `CefUnity.Core.dll` を再ビルドして
  Unity の `Assets/CefUnity/Plugins/` へ配置すること
- Unity 側のコンパイルが通ること (uloop-compile)
- harness で `zero-frame-wait 20 0 1920 1080 intermittent` を実行し、`wait_entered_per_second=0.0`
  / `spin_share=0.0%` を確認する (既定経路が spin ゼロであることの実行時証拠)

Rust 側の変更は無いため `deploy.sh` の再実行は不要。

### 5. ドキュメント

- `docs/HARNESS_MEASUREMENTS.md` の「残作業」リストの #11 を完了に更新し、既定を OFF に
  切り替えたこと・0F が必要になった場合の次の一手 (サーバーからの damage なし通知) を残す
- issue #11 に結果をコメントしてクローズ

## リスク

- **5Hz 程度の間欠更新ページで表示が 1 フレーム遅れる**。実測の 0F 達成率 69% がゼロになるため、
  そうしたページでは最大 16.7ms の追加遅延が出る。ただし received/s とフレーム間ギャップは
  ON/OFF で差が無いことを実測済みで、コマ落ちや供給量の劣化は起きない。必要なら Inspector の
  `_zeroFrameWaitMilliseconds` に正の値を入れるか、`cef_zero_wait` トグルで戻せる
- ログ書式の変更により、`0F-wait:` 行を機械処理している外部スクリプトがあれば影響する。
  リポジトリ内には解析スクリプトが存在しないため実害はない
