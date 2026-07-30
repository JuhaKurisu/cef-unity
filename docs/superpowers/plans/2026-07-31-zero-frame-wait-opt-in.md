# 0F 待ち busy-wait の opt-in 化と計装カウンタ分離 実装計画 (issue #11)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unity メインスレッドの paint 待ち busy-wait を既定 OFF (opt-in) にし、待機回数を正しく数える計装を純 C# クラスとして `CefUnity.Core` に切り出す。

**Architecture:** 待機ロジック (`CefZeroFramePacer`) には手を入れず、(1) `_zeroFrameWaitMilliseconds` の既定を 0 にして早期 return 経路 (Viewer と同じノンブロッキング受信) を既定にし、(2) 集計を新クラス `CefZeroFrameWaitStatistics` へ移して Unity と Harness が共有する。集計をテスト可能な場所へ移すことが、分母バグの再発防止そのものになる。

**Tech Stack:** C# (netstandard2.1 = CefUnity.Core / net10.0 = Tests・Harness)、NUnit 3、Unity 2022 系 MonoBehaviour、`CefUnity.Harness` 診断コマンド

**設計ドキュメント:** `docs/superpowers/specs/2026-07-31-zero-frame-wait-opt-in-design.md`

## Global Constraints

- 識別子は省略形を使わずフルネームで書く (ルート `CLAUDE.md`)。`stats`→`statistics`、`ms` 接尾辞→`Milliseconds`
- `CefUnity.Core` は `netstandard2.1` / `Nullable enable`。C# 10 以降のみの構文 (file-scoped namespace 等) は既存ファイルが使っていないので使わない
- コミットはプレーンな `git commit` のみ。`--author` や `Co-Authored-By` などの trailer を付けない
- `CefZeroFramePacer` の定数群 (`FreshPaintMinDelayMilliseconds` = 4.5f / `NoDamageGiveUpMilliseconds` = 7f / `EarlyPaintAdoptMilliseconds` = 7.5f / `StreakScoreSuppress` = 3 / `SustainedInputFrames` = 3 / `ProbeWindowFrames` = 60) と判定ロジックは**一切変更しない**
- 開発トグルのファイル名は挙動契約。本計画では `cef_no_zero_wait` → `cef_zero_wait` への置換を意図的に行うが、他のトグル名 (`cef_novsync` / `cef_scroll_legacy` / `cef_scroll_record` / `cef_no_streak_cooldown`) には触れない
- **merge 前に harness の性能計測を実行し、結果を `docs/HARNESS_MEASUREMENTS.md` に記録すること** (2026-07-31 のユーザー恒久指示)。Task 4 がこれに当たる

## File Structure

| ファイル | 役割 |
|---|---|
| `cef-unity-csharp/CefUnity.Core/Scroll/CefZeroFrameWaitStatistics.cs` (新規) | 0F 待ちの 1 窓分の集計。受信の成否とパス内訳を別軸で数え、`block_avg` の分母を待機回数に固定する |
| `cef-unity-csharp/CefUnity.Tests/CefZeroFrameWaitStatisticsTests.cs` (新規) | 上のクラスの単体テスト。分母バグの回帰テストを含む |
| `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs` (変更) | 既定値 0 化・トグル反転・自前カウンタ 5 本を統計クラスへ委譲 |
| `cef-unity-csharp/CefUnity.Harness/ZeroFrameWaitCommand.cs` (変更) | `WindowCounters` の共通部分を統計クラスへ置き換え |
| `docs/HARNESS_MEASUREMENTS.md` (変更) | 修正後の計測結果と残作業リストの更新 |

`CefZeroFrameWaitStatistics.cs` を `Scroll/` に置くのは、対になる `CefZeroFramePacer.cs` が同じディレクトリにあるため (0F 待ちはスクロール専用ではないが、既存の配置に合わせる)。

---

### Task 1: `CefZeroFrameWaitStatistics` を `CefUnity.Core` に新設する

**Files:**
- Create: `cef-unity-csharp/CefUnity.Core/Scroll/CefZeroFrameWaitStatistics.cs`
- Test: `cef-unity-csharp/CefUnity.Tests/CefZeroFrameWaitStatisticsTests.cs`

**Interfaces:**
- Consumes: なし (新規・依存なし)
- Produces: `CefUnity.Runtime.CefZeroFrameWaitStatistics` — メソッド `RecordNoWaitReceive(bool)` / `RecordIdleSkip(bool)` / `RecordSuppressedSkip(bool)` / `RecordWaitCompleted(bool, double)` / `Add(CefZeroFrameWaitStatistics)` / `Reset()` / `FormatLine()`、プロパティ `FreshCount` / `FallbackCount` / `NoWaitCount` / `IdleSkipCount` / `SuppressedSkipCount` / `WaitEnteredCount` (すべて `int`)、`SpinTotalMilliseconds` / `SpinMaximumMilliseconds` / `BlockAverageMilliseconds` (すべて `double`)

- [ ] **Step 1: 失敗するテストを書く**

`cef-unity-csharp/CefUnity.Tests/CefZeroFrameWaitStatisticsTests.cs` を新規作成する:

```csharp
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="CefZeroFrameWaitStatistics" /> の単体テスト。
    ///     issue #11 の指摘②「計装カウンタが待機頻度を測れない」の回帰テストを含む:
    ///     旧実装は block_avg の分母に fresh+fallback を使っており、ブロックしない
    ///     抑止スキップパスでもこれらが加算されるため、実測で 3〜15 倍の過小評価になっていた。
    /// </summary>
    public class CefZeroFrameWaitStatisticsTests
    {
        [Test]
        public void NonWaitingPaths_DoNotCountAsWaitEntered()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            statistics.RecordIdleSkip(receivedFreshPaint: false);
            statistics.RecordSuppressedSkip(receivedFreshPaint: true);

            Assert.AreEqual(0, statistics.WaitEnteredCount, "ブロックしないパスは待機回数に入らない");
            Assert.AreEqual(0.0, statistics.SpinTotalMilliseconds, "spin 時間も増えない");
            Assert.AreEqual(1, statistics.NoWaitCount);
            Assert.AreEqual(1, statistics.IdleSkipCount);
            Assert.AreEqual(1, statistics.SuppressedSkipCount);
        }

        [Test]
        public void BlockAverage_UsesWaitEnteredCountAsDenominator()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 6.0);
            statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 8.0);

            Assert.AreEqual(2, statistics.WaitEnteredCount);
            Assert.AreEqual(14.0, statistics.SpinTotalMilliseconds, 1e-9);
            Assert.AreEqual(7.0, statistics.BlockAverageMilliseconds, 1e-9, "spin 合計 ÷ 待機回数");
            Assert.AreEqual(8.0, statistics.SpinMaximumMilliseconds, 1e-9, "最大値を保持する");
        }

        [Test]
        public void BlockAverage_IsNotDilutedBySuppressedSkips()
        {
            // 実測条件の再現: 60 フレーム中 42 が抑止スキップ、18 が実待機で 1 回 7ms。
            var statistics = new CefZeroFrameWaitStatistics();
            for (var frameIndex = 0; frameIndex < 42; frameIndex++)
                statistics.RecordSuppressedSkip(receivedFreshPaint: true);
            for (var frameIndex = 0; frameIndex < 18; frameIndex++)
                statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 7.0);

            Assert.AreEqual(7.0, statistics.BlockAverageMilliseconds, 1e-9,
                "抑止スキップは分母に入れない (旧実装は 126/60 = 2.1ms と表示していた)");
        }

        [Test]
        public void BlockAverage_IsZeroWhenNeverWaited()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            Assert.AreEqual(0.0, statistics.BlockAverageMilliseconds, "0 除算せず 0 を返す");
        }

        [Test]
        public void ReceiveTotals_MatchPathTotals()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordNoWaitReceive(receivedFreshPaint: true);
            statistics.RecordIdleSkip(receivedFreshPaint: true);
            statistics.RecordSuppressedSkip(receivedFreshPaint: false);
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 3.0);

            var receiveTotal = statistics.FreshCount + statistics.FallbackCount;
            var pathTotal = statistics.NoWaitCount + statistics.IdleSkipCount
                          + statistics.SuppressedSkipCount + statistics.WaitEnteredCount;
            Assert.AreEqual(4, receiveTotal, "全パスで受信の成否を数える");
            Assert.AreEqual(receiveTotal, pathTotal, "2 つの軸の合計はどちらもフレーム数");
            Assert.AreEqual(3, statistics.FreshCount);
            Assert.AreEqual(1, statistics.FallbackCount);
        }

        [Test]
        public void Add_AccumulatesWindowsIntoTotals()
        {
            var window = new CefZeroFrameWaitStatistics();
            window.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 4.0);
            window.RecordSuppressedSkip(receivedFreshPaint: false);

            var totals = new CefZeroFrameWaitStatistics();
            totals.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 9.0);
            totals.Add(window);

            Assert.AreEqual(2, totals.WaitEnteredCount);
            Assert.AreEqual(1, totals.SuppressedSkipCount);
            Assert.AreEqual(13.0, totals.SpinTotalMilliseconds, 1e-9);
            Assert.AreEqual(9.0, totals.SpinMaximumMilliseconds, 1e-9, "最大値は大きい方が残る");
            Assert.AreEqual(2, totals.FreshCount);
            Assert.AreEqual(1, totals.FallbackCount);
        }

        [Test]
        public void Reset_ClearsEveryCounter()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordWaitCompleted(receivedFreshPaint: true, spinMilliseconds: 5.0);
            statistics.RecordIdleSkip(receivedFreshPaint: false);
            statistics.Reset();

            Assert.AreEqual(0, statistics.FreshCount);
            Assert.AreEqual(0, statistics.FallbackCount);
            Assert.AreEqual(0, statistics.NoWaitCount);
            Assert.AreEqual(0, statistics.IdleSkipCount);
            Assert.AreEqual(0, statistics.SuppressedSkipCount);
            Assert.AreEqual(0, statistics.WaitEnteredCount);
            Assert.AreEqual(0.0, statistics.SpinTotalMilliseconds);
            Assert.AreEqual(0.0, statistics.SpinMaximumMilliseconds);
        }

        [Test]
        public void FormatLine_ContainsSeparatedCounters()
        {
            var statistics = new CefZeroFrameWaitStatistics();
            statistics.RecordSuppressedSkip(receivedFreshPaint: true);
            statistics.RecordWaitCompleted(receivedFreshPaint: false, spinMilliseconds: 7.0);

            var line = statistics.FormatLine();
            StringAssert.Contains("wait_entered=1", line);
            StringAssert.Contains("suppressed_skip=1", line);
            StringAssert.Contains("block_avg=7.00ms", line);
        }
    }
}
```

- [ ] **Step 2: テストが失敗することを確認する**

```bash
dotnet test cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj --filter FullyQualifiedName~CefZeroFrameWaitStatisticsTests
```

Expected: ビルドエラー `CS0246: The type or namespace name 'CefZeroFrameWaitStatistics' could not be found`

- [ ] **Step 3: 実装を書く**

`cef-unity-csharp/CefUnity.Core/Scroll/CefZeroFrameWaitStatistics.cs` を新規作成する:

```csharp
using System;
using System.Globalization;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     0F 待ち (<see cref="CefZeroFramePacer" />) の 1 窓分の集計。純 C# (Unity API 非依存) で、
    ///     Unity の <c>CefUnityBrowserSample</c> と <c>CefUnity.Harness</c> の診断コマンドが共有する。
    ///
    ///     カウンタを 2 つの軸に分けるのが要点 (issue #11 の指摘②):
    ///     <list type="bullet">
    ///       <item>受信の成否 — <see cref="FreshCount" /> / <see cref="FallbackCount" />。
    ///             4 つの Record メソッドすべてで加算され、合計は recv を試みたフレーム数になる</item>
    ///       <item>通ったパス — <see cref="NoWaitCount" /> / <see cref="IdleSkipCount" /> /
    ///             <see cref="SuppressedSkipCount" /> / <see cref="WaitEnteredCount" />。
    ///             対応する 1 つの Record メソッドでのみ加算され、合計は同じフレーム数になる</item>
    ///     </list>
    ///     旧実装は <c>block_avg</c> の分母に受信の成否軸 (fresh+fallback) を使っていたため、
    ///     ブロックしない抑止スキップパスが分母を膨らませ、実測で 3〜15 倍の過小評価になっていた。
    ///     <see cref="BlockAverageMilliseconds" /> の分母は必ず <see cref="WaitEnteredCount" /> にする。
    /// </summary>
    public sealed class CefZeroFrameWaitStatistics
    {
        /// <summary>新 paint を取得できた回数 (全パス合計)。</summary>
        public int FreshCount { get; private set; }

        /// <summary>取得できず前フレーム内容を継続表示した回数 (全パス合計)。</summary>
        public int FallbackCount { get; private set; }

        /// <summary>待ちが無効 (既定) でノンブロッキング受信のみ行った回数。</summary>
        public int NoWaitCount { get; private set; }

        /// <summary>プローブ判定 (静止中) で待ちをスキップした回数。</summary>
        public int IdleSkipCount { get; private set; }

        /// <summary>damage streak 抑止推定・連続入力で待ちをスキップした回数。</summary>
        public int SuppressedSkipCount { get; private set; }

        /// <summary>実際に busy-wait ループへ入った回数。</summary>
        public int WaitEnteredCount { get; private set; }

        /// <summary>busy-wait に費やした時間の合計。</summary>
        public double SpinTotalMilliseconds { get; private set; }

        /// <summary>1 回の busy-wait の最大時間。</summary>
        public double SpinMaximumMilliseconds { get; private set; }

        /// <summary>待機 1 回あたりの平均ブロック時間。分母は待機回数のみ。</summary>
        public double BlockAverageMilliseconds
            => WaitEnteredCount > 0 ? SpinTotalMilliseconds / WaitEnteredCount : 0.0;

        /// <summary>待ち無効時のノンブロッキング受信を記録する。</summary>
        public void RecordNoWaitReceive(bool receivedFreshPaint)
        {
            RecordReceive(receivedFreshPaint);
            NoWaitCount++;
        }

        /// <summary>プローブ判定による待ちスキップを記録する。</summary>
        public void RecordIdleSkip(bool receivedFreshPaint)
        {
            RecordReceive(receivedFreshPaint);
            IdleSkipCount++;
        }

        /// <summary>抑止推定・連続入力による待ちスキップを記録する。</summary>
        public void RecordSuppressedSkip(bool receivedFreshPaint)
        {
            RecordReceive(receivedFreshPaint);
            SuppressedSkipCount++;
        }

        /// <summary>
        ///     busy-wait を 1 回完了したことを記録する。待機回数と spin 時間を必ず同じ呼び出しで
        ///     入れることで、待機に入ったのに数え忘れる / ブロックしないパスで数える事故を防ぐ。
        /// </summary>
        public void RecordWaitCompleted(bool receivedFreshPaint, double spinMilliseconds)
        {
            RecordReceive(receivedFreshPaint);
            WaitEnteredCount++;
            SpinTotalMilliseconds += spinMilliseconds;
            if (spinMilliseconds > SpinMaximumMilliseconds) SpinMaximumMilliseconds = spinMilliseconds;
        }

        /// <summary>窓の集計を累計へ足し込む (最大値は大きい方を採る)。</summary>
        public void Add(CefZeroFrameWaitStatistics other)
        {
            if (other == null) throw new ArgumentNullException(nameof(other));
            FreshCount += other.FreshCount;
            FallbackCount += other.FallbackCount;
            NoWaitCount += other.NoWaitCount;
            IdleSkipCount += other.IdleSkipCount;
            SuppressedSkipCount += other.SuppressedSkipCount;
            WaitEnteredCount += other.WaitEnteredCount;
            SpinTotalMilliseconds += other.SpinTotalMilliseconds;
            if (other.SpinMaximumMilliseconds > SpinMaximumMilliseconds)
                SpinMaximumMilliseconds = other.SpinMaximumMilliseconds;
        }

        /// <summary>全カウンタを 0 に戻す (1 秒窓の切り替え時に呼ぶ)。</summary>
        public void Reset()
        {
            FreshCount = 0;
            FallbackCount = 0;
            NoWaitCount = 0;
            IdleSkipCount = 0;
            SuppressedSkipCount = 0;
            WaitEnteredCount = 0;
            SpinTotalMilliseconds = 0.0;
            SpinMaximumMilliseconds = 0.0;
        }

        /// <summary>1 行のログ表現 (呼び出し側が接頭辞を付ける)。</summary>
        public string FormatLine()
            => string.Format(CultureInfo.InvariantCulture,
                "fresh={0} fallback(1F)={1} no_wait={2} idle={3} suppressed_skip={4} " +
                "wait_entered={5} block_avg={6:F2}ms block_max={7:F2}ms",
                FreshCount, FallbackCount, NoWaitCount, IdleSkipCount, SuppressedSkipCount,
                WaitEnteredCount, BlockAverageMilliseconds, SpinMaximumMilliseconds);

        private void RecordReceive(bool receivedFreshPaint)
        {
            if (receivedFreshPaint) FreshCount++;
            else FallbackCount++;
        }
    }
}
```

- [ ] **Step 4: テストが通ることを確認する**

```bash
dotnet test cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj --filter FullyQualifiedName~CefZeroFrameWaitStatisticsTests
```

Expected: PASS (8 テスト)

- [ ] **Step 5: 既存テストの回帰がないことを確認する**

```bash
dotnet test cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj
```

Expected: 全件 PASS (`CefZeroFramePacerTests` を含む既存テストが緑のまま)

- [ ] **Step 6: コミットする**

```bash
git add cef-unity-csharp/CefUnity.Core/Scroll/CefZeroFrameWaitStatistics.cs cef-unity-csharp/CefUnity.Tests/CefZeroFrameWaitStatisticsTests.cs
git commit -m "feat(core): 0F 待ちの集計を CefZeroFrameWaitStatistics に切り出す (issue #11)"
```

---

### Task 2: Unity 側を既定 OFF にして統計クラスへ委譲する

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs:97-110` (フィールド定義)、`:207-211` (開発トグル)、`:395-406` (ログ出力)、`:576-625` (`ReceiveBeforeRender`)

**Interfaces:**
- Consumes: Task 1 の `CefZeroFrameWaitStatistics` (`RecordNoWaitReceive` / `RecordIdleSkip` / `RecordSuppressedSkip` / `RecordWaitCompleted` / `Reset` / `FormatLine`)
- Produces: なし (このタスクで完結)

- [ ] **Step 1: `CefUnity.Core.dll` を再ビルドして Unity へ配置する**

Unity は `Assets/CefUnity/Plugins/CefUnity.Core.dll` を参照するため、Task 1 の新クラスを Unity から見えるようにする:

```bash
bash cef-unity-csharp/build-csharp.sh
```

Expected: `copied CefUnity.Core.dll -> .../Assets/CefUnity/Plugins`

- [ ] **Step 2: フィールド定義を差し替える**

`CefUnityBrowserSample.cs` の 97-110 行目、Tooltip・既定値・カウンタ 5 本を次で置き換える:

```csharp
        [SerializeField, Tooltip("BF#1 発行からこの時間 (ms) までは flush 結果の到着を待って 0F 化する。" +
            "既定 0 = 無効 (常にノンブロッキング受信)。正の値を入れると 0F 化と引き換えに " +
            "メインスレッドが最大この時間 busy-wait する — 実測では間欠更新ページで実時間の 42.5% を " +
            "spin に使い CPU が約 25 倍になるため、必要な場面でのみ opt-in すること " +
            "(docs/HARNESS_MEASUREMENTS.md の #11)。")]
        [FormerlySerializedAs("_zeroFrameWaitMs")]
        private float _zeroFrameWaitMilliseconds = 0f;
        // 待ち判定の状態機械 (定数・streak 推定・プローブ窓は CefZeroFramePacer に集約)。
        private readonly CefZeroFramePacer _pacer = new CefZeroFramePacer();
        // このフレームで CEF へ入力イベントを送ったか (アクティブ判定の即時トリガー)。
        private bool _inputSentThisFrame;
        // 0F 待ち検証メトリクス (集計は CefZeroFrameWaitStatistics に集約。待機パス専用の
        // wait_entered を分母にするため、抑止スキップが block_avg を薄めない)。
        private readonly CefZeroFrameWaitStatistics _zeroFrameWaitStatistics = new CefZeroFrameWaitStatistics();
```

- [ ] **Step 3: 開発トグルを反転する**

同ファイル 207-211 行目を次で置き換える:

```csharp
                // 開発トグル: cef_zero_wait マーカーで 0F 待ちを有効化 (既定は OFF、A/B 比較用)。
                // シーンの serialized 値は Editor が外部変更を再読込しないため、既存の開発
                // トグル群と同じ temp ファイル方式で切り替える。
                if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_zero_wait")))
                    _zeroFrameWaitMilliseconds = 10f;
```

- [ ] **Step 4: ログ出力を統計クラスへ委譲する**

同ファイル 395-406 行目を次で置き換える:

```csharp
                    // 0F 待ち専用メトリクス (待ちを有効にしたときだけ出力する)。
                    if (_zeroFrameWaitMilliseconds > 0f && _useAcceleratedPaint)
                    {
                        CefLog.Log($"[CefUnity] 0F-wait: {_zeroFrameWaitStatistics.FormatLine()}");
                        _zeroFrameWaitStatistics.Reset();
                    }
```

- [ ] **Step 5: `ReceiveBeforeRender` の記録呼び出しを差し替える**

同ファイルの `ReceiveBeforeRender` 本体 (576-625 行目) を次で置き換える。`blockStart` は待機パスでのみ使うので宣言位置を待機の直前へ移す:

```csharp
        private void ReceiveBeforeRender()
        {
            // software 経路 / 待ち無効時 (既定) は従来のノンブロッキング受信のみ。
            if (!_useAcceleratedPaint || _zeroFrameWaitMilliseconds <= 0f)
            {
                var receivedWithoutWait = TryUpdateTextureOnce();
                if (receivedWithoutWait) OnFreshPaint(); else OnNoPaint();
                _zeroFrameWaitStatistics.RecordNoWaitReceive(receivedWithoutWait);
                return;
            }

            // プローブ判定 (静止中は待たない)。判定根拠は CefZeroFramePacer 参照。
            if (_pacer.ShouldSkipAsIdle(_inputSentThisFrame))
            {
                var receivedWhileIdle = TryUpdateTextureOnce();
                if (receivedWhileIdle) OnFreshPaint(); else OnNoPaint();
                _zeroFrameWaitStatistics.RecordIdleSkip(receivedWhileIdle);
                return;
            }

            // damage streak 抑止推定・連続入力中は待ちスキップ (根拠は CefZeroFramePacer 参照)。
            if (_pacer.ShouldSkipAsSuppressed())
            {
                var receivedWhileSuppressed = TryUpdateTextureOnce();
                if (receivedWhileSuppressed) OnFreshPaint(); else OnNoPaint();
                _zeroFrameWaitStatistics.RecordSuppressedSkip(receivedWhileSuppressed);
                return;
            }

            var blockStart = Time.realtimeSinceStartup;
            var window = _pacer.OpenWaitWindow(_zeroFrameWaitMilliseconds);
            while (true)
            {
                var now = Time.realtimeSinceStartup;
                if (window.DeadlineReached(now)) break;
                if (window.OnAcceleratedFrameIdSample(now, _browser.PeekAcceleratedFrameId())) break;
                // Peek (FFI + SHM read) のフル回転を避けて CPU/メモリバス圧を下げる。
                // SpinWait はデスケジュールされない (Thread.Sleep(1) は macOS で 10ms+
                // オーバースリープするため使用不可)。時間精度は ~µs で十分。
                System.Threading.Thread.SpinWait(64);
            }

            // 増分で抜けた場合はその paint を、デッドライン切れでも直前に届いた分があれば拾う
            // (TryReceive は queue を drain して最新を返すため、どちらでも最新が取れる)。
            var receivedAfterWait = TryUpdateTextureOnce();
            if (receivedAfterWait) OnFreshPaint(); else OnNoPaint();
            _zeroFrameWaitStatistics.RecordWaitCompleted(
                receivedAfterWait, (Time.realtimeSinceStartup - blockStart) * 1000.0);
        }
```

- [ ] **Step 6: クラスコメントを実態に合わせる**

同ファイルの `ReceiveBeforeRender` の XML コメント (567-575 行目) の冒頭に、既定が OFF であることを追記する。既存の説明文は待ちを有効にした場合の説明として残す:

```csharp
        /// <summary>
        /// 描画発行前の recv 本体。既定 (_zeroFrameWaitMilliseconds = 0) では待たずに
        /// ノンブロッキング受信のみを行う (CefUnity.Viewer の CefFrameSource と同じ挙動)。
        /// 正の値を設定した場合のみ以下の 0F 待ちが有効になる:
        /// server-side flush の結果 (accel_frame_id 増分) を _zeroFrameWaitMilliseconds
        /// (BF#1 からの経過時間 cap) まで待ち、届いた最新 paint を同フレームの present に
        /// 乗せる (0F)。ゲーム処理が重いフレームではここへの到達が遅く cap を過ぎているため
        /// 自動的に待ちゼロ (flush 結果は自然に到着済み)。デッドラインまでに届かなければ
        /// 従来通り 1F フォールバック。待ちは SHM カウンタの busy-wait のみで IPC を発行しない
        /// (旧 client-side double-pump の reflush による IPC フラッディング → 46ms ブロック問題は
        /// 構造的に発生しない)。
        /// </summary>
```

- [ ] **Step 7: Unity のコンパイルが通ることを確認する**

uloop の compile スキルを使う (`uloop-compile`)。Unity Editor が起動していない場合は先に `uloop-launch` する。

Expected: errors=0。`_doublePumpFreshCount` 等の旧フィールドへの参照が残っていればここで検出される

- [ ] **Step 8: コミットする**

```bash
git add cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs
git commit -m "fix(unity): 0F 待ち busy-wait を既定 OFF の opt-in にする (issue #11)"
```

---

### Task 3: Harness の `WindowCounters` を統計クラスへ置き換える

**Files:**
- Modify: `cef-unity-csharp/CefUnity.Harness/ZeroFrameWaitCommand.cs:55-70` (`WindowCounters`)、`:151-199` (窓の集計とログ行)、`:206-216` (`CLIENT_SUMMARY`)、`:234-280` (`ReceiveBeforeRender`)

**Interfaces:**
- Consumes: Task 1 の `CefZeroFrameWaitStatistics` (`Record*` / `Add` / `BlockAverageMilliseconds` / 各カウンタ)
- Produces: なし (このタスクで完結)

- [ ] **Step 1: `WindowCounters` を統計クラス委譲に変える**

`ZeroFrameWaitCommand.cs` の `WindowCounters` 定義 (55-70 行目) を次で置き換える。共通部分は統計クラスへ移し、harness 固有の項目だけを残す:

```csharp
    private sealed class WindowCounters
    {
        /// <summary>Unity と共有する集計 (受信の成否・パス内訳・spin)。</summary>
        public readonly CefZeroFrameWaitStatistics Statistics = new CefZeroFrameWaitStatistics();
        public int ReceivedCount;
        public double MaximumGapMilliseconds;
        // 待機の目的側: 受け取った paint が何フレーム前の BeginFrame 由来か (0 = 同フレーム = 0F)。
        public int ZeroFrameDelayCount;   // 遅延 0 フレーム
        public int OneFrameDelayCount;    // 遅延 1 フレーム
        public int LaterFrameDelayCount;  // 遅延 2 フレーム以上
    }
```

- [ ] **Step 2: `ReceiveBeforeRender` の記録呼び出しを差し替える**

同ファイル 234-280 行目の `ReceiveBeforeRender` を次で置き換える:

```csharp
    /// <summary>
    ///     CefUnityBrowserSample.ReceiveBeforeRender の忠実移植。集計は Unity と同じ
    ///     CefZeroFrameWaitStatistics に委譲するため、カウンタの加算位置も自動的に一致する。
    /// </summary>
    private static void ReceiveBeforeRender(Browser browser, CefZeroFramePacer pacer,
                                            Stopwatch clock, float zeroFrameWaitMilliseconds,
                                            WindowCounters counters, ulong beginFrameIndexThisFrame)
    {
        if (zeroFrameWaitMilliseconds <= 0f)
        {
            counters.Statistics.RecordNoWaitReceive(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        if (pacer.ShouldSkipAsIdle(inputSentThisFrame: false))
        {
            counters.Statistics.RecordIdleSkip(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        if (pacer.ShouldSkipAsSuppressed())
        {
            counters.Statistics.RecordSuppressedSkip(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        // ここから先が実際の busy-wait。#11 が数えたい対象。
        var blockStart = ElapsedSeconds(clock);
        var window = pacer.OpenWaitWindow(zeroFrameWaitMilliseconds);
        while (true)
        {
            var now = ElapsedSeconds(clock);
            if (window.DeadlineReached(now)) break;
            if (window.OnAcceleratedFrameIdSample(now, browser.PeekAcceleratedFrameId())) break;
            Thread.SpinWait(64);
        }

        var receivedAfterWait = Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame);
        counters.Statistics.RecordWaitCompleted(
            receivedAfterWait, (ElapsedSeconds(clock) - blockStart) * 1000.0);
    }
```

- [ ] **Step 3: 1 秒窓のログ行を差し替える**

同ファイル 151-199 行目の窓集計ブロックを次で置き換える。`block_avg_old` (旧分母) の表示は役目を終えたので落とし、統計クラスの `FormatLine` に寄せる:

```csharp
                if (clock.Elapsed - windowStart >= TimeSpan.FromSeconds(1))
                {
                    var processorTime = process.TotalProcessorTime;
                    var processorMilliseconds =
                        (processorTime - processorTimeAtWindowStart).TotalMilliseconds;
                    processorTimeAtWindowStart = processorTime;

                    var windowMilliseconds = (clock.Elapsed - windowStart).TotalMilliseconds;
                    var statistics = counters.Statistics;

                    var line =
                        $"t={clock.Elapsed.TotalSeconds,5:F1}s received={counters.ReceivedCount,3} " +
                        $"max_gap={counters.MaximumGapMilliseconds,6:F1}ms | " +
                        $"{statistics.FormatLine()} | " +
                        $"spin_total={statistics.SpinTotalMilliseconds,6:F1}ms " +
                        $"({statistics.SpinTotalMilliseconds / windowMilliseconds * 100,4:F1}% of wall) | " +
                        $"delay_0F={counters.ZeroFrameDelayCount,3} delay_1F={counters.OneFrameDelayCount,3} " +
                        $"delay_2F+={counters.LaterFrameDelayCount,3} | " +
                        $"process_cpu={processorMilliseconds,6:F0}ms";
                    Console.WriteLine(line);
                    summaries.Add(line);

                    totals.Statistics.Add(statistics);
                    totals.ReceivedCount += counters.ReceivedCount;
                    totals.ZeroFrameDelayCount += counters.ZeroFrameDelayCount;
                    totals.OneFrameDelayCount += counters.OneFrameDelayCount;
                    totals.LaterFrameDelayCount += counters.LaterFrameDelayCount;
                    totals.MaximumGapMilliseconds =
                        Math.Max(totals.MaximumGapMilliseconds, counters.MaximumGapMilliseconds);
                    totalSeconds++;

                    counters = new WindowCounters();
                    windowStart = clock.Elapsed;
                }
```

- [ ] **Step 4: `CLIENT_SUMMARY` を差し替える**

同ファイル 206-216 行目のサマリ出力を次で置き換える:

```csharp
            Console.WriteLine();
            Console.WriteLine(
                $"CLIENT_SUMMARY zero_frame_wait={zeroFrameWaitMilliseconds:F1}ms seconds={totalSeconds} " +
                $"received_per_second={(totalSeconds > 0 ? (double)totals.ReceivedCount / totalSeconds : 0):F1} " +
                $"gap_max={totals.MaximumGapMilliseconds:F1}ms " +
                $"wait_entered_per_second={(totalSeconds > 0 ? (double)totals.Statistics.WaitEnteredCount / totalSeconds : 0):F1} " +
                $"spin_share={(totalSeconds > 0 ? totals.Statistics.SpinTotalMilliseconds / (totalSeconds * 1000.0) * 100 : 0):F1}% " +
                $"spin_max={totals.Statistics.SpinMaximumMilliseconds:F2}ms " +
                $"block_avg_entered={totals.Statistics.BlockAverageMilliseconds:F2}ms " +
                $"delay_0F={totals.ZeroFrameDelayCount} delay_1F={totals.OneFrameDelayCount} " +
                $"delay_2F+={totals.LaterFrameDelayCount} " +
                $"zero_frame_share={(totals.ReceivedCount > 0 ? (double)totals.ZeroFrameDelayCount / totals.ReceivedCount * 100 : 0):F1}%");
```

- [ ] **Step 5: クラスコメントから「カウンタ分離をここで行う」の記述を直す**

同ファイル 21-27 行目のコメント段落を次で置き換える (分離は Core 側で恒久化されたため):

```csharp
    ///     <para>
    ///     issue #11 が求める「待機パス専用カウンタの分離」は <see cref="CefZeroFrameWaitStatistics" />
    ///     として Core 側で恒久化した。本コマンドは Unity と同じそのクラスへ集計を委譲するので、
    ///     カウンタの加算位置は Unity と常に一致する。
    ///     </para>
```

- [ ] **Step 6: ビルドが通ることを確認する**

```bash
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug
```

Expected: `Build succeeded` / 警告 0 件 (`SpinMaxMilliseconds` など旧フィールドへの参照が残っていればここで検出される)

- [ ] **Step 7: コミットする**

```bash
git add cef-unity-csharp/CefUnity.Harness/ZeroFrameWaitCommand.cs
git commit -m "refactor(harness): 0F 待ちの集計を CefZeroFrameWaitStatistics へ委譲する (issue #11)"
```

---

### Task 4: 性能計測・記録・issue クローズ

**Files:**
- Modify: `docs/HARNESS_MEASUREMENTS.md` (結果一覧 44 行目・#11 セクション 196-217 行目・残作業リスト 276-295 行目)

**Interfaces:**
- Consumes: Task 3 でビルドした `CefUnity.Harness`
- Produces: なし

- [ ] **Step 1: 計測環境を整える**

CEF プロセスの残留があると計測がぶれるので落としてから始める:

```bash
pkill -f cef-unity-server || true
uptime
```

Expected: `load average` が 5 未満であること (高負荷だと spin 比率が実態より悪化する。`docs/HARNESS_MEASUREMENTS.md` の「計測上の注意」参照)

- [ ] **Step 2: 既定経路 (待ち OFF) を計測する**

```bash
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
./CefUnity.Harness zero-frame-wait 20 0 1920 1080 intermittent
```

Expected: `wait_entered_per_second=0.0` / `spin_share=0.0%` / 各窓の `no_wait=60` 付近。これが「既定で busy-wait が消えた」実行時証拠になる

- [ ] **Step 3: opt-in 経路が壊れていないことを確認する**

```bash
./CefUnity.Harness zero-frame-wait 20 10 1920 1080 intermittent
```

Expected: `wait_entered_per_second` が 55〜60 / `spin_share` が 35〜45% / `zero_frame_share` が 60〜75%。修正前の実測 (wait_entered 60・spin 42.5%・0F 69%) と同水準であること。新しい `block_avg` が 6.5〜9.5ms を示すこと (旧 `block_avg_old` の 0.12〜3.73ms ではない)

- [ ] **Step 4: paint 供給の回帰がないことを確認する**

```bash
./CefUnity.Harness paint-statistics 20 1920 1080 animation
```

Expected: `paints/s` が 60 付近、`BeginFrame→paint` が 3〜4ms。#7 修正後の水準から悪化していないこと

- [ ] **Step 5: ライフサイクルの回帰がないことを確認する**

```bash
./CefUnity.Harness lifecycle 5
```

Expected: 5 サイクル完走。`mach_ports` の増加傾向が #10 の既知の水準 (+1〜2/サイクル) から悪化していないこと

- [ ] **Step 6: 結果を `docs/HARNESS_MEASUREMENTS.md` に記録する**

3 箇所を更新する:

1. 結果一覧 (44 行目) の #11 の行を `✅ **修正済**` にし、実測欄に「既定 OFF 化で spin 0%、opt-in 時のみ従来動作」を書く
2. `## #11` セクション (196-217 行目) の末尾に「### 修正後 (2026-07-31)」を追加し、Step 2〜5 で得た数値をそのまま貼る。修正内容 (既定 0 の opt-in 化・`cef_zero_wait` トグル・カウンタ分離の Core 恒久化) を 3 行で書く
3. 残作業リスト (276-295 行目) の 7 番 (#11) を完了に更新し、0F を取り戻す場合の次の一手 (サーバーからの damage なし通知) が未着手であることを残す

- [ ] **Step 7: ドキュメントをコミットする**

```bash
git add docs/HARNESS_MEASUREMENTS.md
git commit -m "docs: 0F 待ち opt-in 化後の計測結果を記録する (issue #11)"
```

- [ ] **Step 8: main へマージして issue をクローズする**

```bash
git checkout main && git merge --no-ff JuhaKurisu/ribbonworm && git push origin main
gh issue close 11 --comment "既定を OFF (opt-in) にし、計装カウンタを CefZeroFrameWaitStatistics として Core へ分離しました。計測結果は docs/HARNESS_MEASUREMENTS.md の #11 セクションを参照してください。"
```

---

## Self-Review

**Spec coverage:**

| spec の要求 | 対応タスク |
|---|---|
| 既定値 `10f` → `0f` | Task 2 Step 2 |
| Tooltip 更新 | Task 2 Step 2 |
| 開発トグル `cef_no_zero_wait` → `cef_zero_wait` 反転 | Task 2 Step 3 |
| `CefZeroFrameWaitStatistics` 新設 (7 項目 + `NoWaitCount`) | Task 1 Step 3 |
| `BlockAverageMilliseconds` の分母を `WaitEnteredCount` に | Task 1 Step 3 + テスト Step 1 |
| 加算規則の 2 軸 (受信の成否 / パス内訳) | Task 1 Step 3 + `ReceiveTotals_MatchPathTotals` |
| ログ行に `wait_entered` / `suppressed_skip` 追加 | Task 1 Step 3 (`FormatLine`) + Task 2 Step 4 |
| Unity が自前カウンタ 5 本を捨てて委譲 | Task 2 Step 2・Step 5 |
| Harness の `WindowCounters` を置き換え | Task 3 Step 1〜4 |
| テスト 7 項目 | Task 1 Step 1 (8 テストで網羅) |
| `dotnet test` 全緑 | Task 1 Step 5 |
| `build-csharp.sh` で Core.dll 配置 | Task 2 Step 1 |
| Unity コンパイル確認 | Task 2 Step 7 |
| harness で既定経路の spin 0 を確認 | Task 4 Step 2 |
| `docs/HARNESS_MEASUREMENTS.md` 更新 | Task 4 Step 6 |
| issue クローズ | Task 4 Step 8 |

`CefZeroFramePacer` 無変更・Rust 無変更 (deploy 不要) も守られている。spec に無い追加は `Add()` メソッド 1 つで、これは Harness の累計集計に必要 (Task 3 Step 3 が使用)。

**Placeholder scan:** 「TBD」「後で」「適宜」の類は無し。すべてのコードステップに実コードがある。

**Type consistency:** `CefZeroFrameWaitStatistics` のメソッド名・引数名 (`receivedFreshPaint` / `spinMilliseconds`) は Task 1 の定義と Task 2・3 の呼び出しで一致。プロパティ名 `SpinMaximumMilliseconds` / `SpinTotalMilliseconds` / `WaitEnteredCount` / `BlockAverageMilliseconds` も全タスクで統一。Harness 側で `counters.Statistics` 経由に変わる点は Task 3 Step 1 の定義と Step 3・4 の参照で一致している。
