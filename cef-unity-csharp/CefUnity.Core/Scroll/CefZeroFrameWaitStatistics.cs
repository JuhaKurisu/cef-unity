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
