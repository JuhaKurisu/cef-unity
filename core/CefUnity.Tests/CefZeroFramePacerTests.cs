using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="CefZeroFramePacer" /> / <see cref="ZeroFrameWaitWindow" /> の単体テスト。
    ///     0F 待ちの判定 (プローブ窓・streak 抑止推定・連続入力スキップ・busy-wait の
    ///     4 分岐: fresh 検知 / stale 読み捨て / earlyAdopt / noDamageGiveUp) を検証する。
    ///     定数は実測チューニング値 (REFACTORING_REPORT.md §1) — 値の変更はテスト側も
    ///     追従が必要で、その際はスクロール実測での回帰確認とセットで行うこと。
    /// </summary>
    public class CefZeroFramePacerTests
    {
        private const float Bf1 = 100f;      // 基準時刻 (秒)
        private const float WaitMs = 10f;    // _zeroFrameWaitMs 既定

        private static float Ms(float ms) => Bf1 + ms * 0.001f;

        // ---- プローブ判定 (ShouldSkipAsIdle) ----

        [Test]
        public void Idle_WhenNeverPainted_AndNoInput()
        {
            var p = new CefZeroFramePacer();
            Assert.IsTrue(p.ShouldSkipAsIdle(inputSentThisFrame: false), "初期状態 (paint 未取得) は静止扱い");
            Assert.IsFalse(p.ShouldSkipAsIdle(inputSentThisFrame: true), "入力を送ったフレームは待つ");
        }

        [Test]
        public void ProbeWindow_StaysActiveFor60FramesAfterFreshPaint()
        {
            var p = new CefZeroFramePacer();
            p.OnFreshPaint();
            // 59 回受信なしでも窓内 (framesSince=59 < 60)
            for (var i = 0; i < 59; i++) p.OnNoPaint();
            Assert.IsFalse(p.ShouldSkipAsIdle(false), "fresh から 59F は窓内 = 待つ");
            p.OnNoPaint();
            Assert.IsTrue(p.ShouldSkipAsIdle(false), "60F で窓を超え静止扱い");
        }

        // ---- streak 抑止推定 (ShouldSkipAsSuppressed) ----

        [Test]
        public void StreakScore_SuppressesAfter3ConsecutiveFresh()
        {
            var p = new CefZeroFramePacer();
            p.OnFreshPaint();
            p.OnFreshPaint();
            Assert.IsFalse(p.ShouldSkipAsSuppressed(), "スコア 2 では非抑止");
            p.OnFreshPaint();
            Assert.IsTrue(p.ShouldSkipAsSuppressed(), "スコア 3 で抑止推定");
        }

        [Test]
        public void StreakScore_HysteresisSurvivesOneMiss()
        {
            var p = new CefZeroFramePacer();
            for (var i = 0; i < 5; i++) p.OnFreshPaint(); // スコア 5
            p.OnNoPaint(); // -2 → 3
            Assert.IsTrue(p.ShouldSkipAsSuppressed(), "1 回の取り逃しでは抑止推定を維持 (ヒステリシス)");
            p.OnNoPaint(); // -2 → 1
            Assert.IsFalse(p.ShouldSkipAsSuppressed(), "2 連続で外れたら解除");
        }

        [Test]
        public void StreakScore_CapsAtMax_ForFastRelease()
        {
            var p = new CefZeroFramePacer();
            for (var i = 0; i < 100; i++) p.OnFreshPaint(); // 天井 6 で頭打ち
            // 6 → -2×2 = 2 (< 3) で解除される (天井が高いと解除が遅れる)
            p.OnNoPaint();
            p.OnNoPaint();
            Assert.IsFalse(p.ShouldSkipAsSuppressed(), "長時間スクロール後も 2 フレームで解除");
        }

        // ---- 連続入力スキップ ----

        [Test]
        public void SustainedInput_SkipsAfter3ConsecutiveInputFrames()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, 0, inputSentThisFrame: true);
            p.OnBeginFrame(Bf1, 0, inputSentThisFrame: true);
            Assert.IsFalse(p.ShouldSkipAsSuppressed(), "連続 2 フレームでは待つ (単発入力は 0F を取る)");
            p.OnBeginFrame(Bf1, 0, inputSentThisFrame: true);
            Assert.IsTrue(p.ShouldSkipAsSuppressed(), "連続 3 フレームでスキップ");
            p.OnBeginFrame(Bf1, 0, inputSentThisFrame: false);
            Assert.IsFalse(p.ShouldSkipAsSuppressed(), "入力が途切れたら即リセット");
        }

        // ---- busy-wait 窓 (ZeroFrameWaitWindow) ----

        [Test]
        public void Wait_FreshPaintAfterMinDelay_EndsWait()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            // 4.5ms (FreshPaintMinDelayMs) 以降の増分 = fresh (#B) → 即終了
            Assert.IsFalse(w.OnAfiSample(Ms(2f), 5), "増分なしは継続");
            Assert.IsTrue(w.OnAfiSample(Ms(5f), 6), "freshMinTime 後の増分で待ち終了");
        }

        [Test]
        public void Wait_StalePaintBeforeMinDelay_IsSkippedThenFreshTaken()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            // 4.5ms より前の増分 = BF#1 由来 stale (#A) → 読み捨てて継続
            Assert.IsFalse(w.OnAfiSample(Ms(2f), 6), "stale (#A) は読み捨てて待ち続行");
            // その後 fresh (#B) が来たら終了
            Assert.IsTrue(w.OnAfiSample(Ms(6f), 7), "fresh (#B) で終了");
        }

        [Test]
        public void Wait_EarlyPaintWithoutFresh_AdoptsAtEarlyAdoptTime()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            Assert.IsFalse(w.OnAfiSample(Ms(2f), 6), "stale (#A) 読み捨て");
            // #B が来ないまま 7.5ms (EarlyPaintAdoptMs) 到達 → #A を採用して終了
            Assert.IsFalse(w.OnAfiSample(Ms(7f), 6), "earlyAdopt 前は粘る");
            Assert.IsTrue(w.OnAfiSample(Ms(7.6f), 6), "earlyAdopt で #A 採用");
        }

        [Test]
        public void Wait_NoDamage_GivesUpAt7ms()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            // 増分ゼロのまま 7ms (NoDamageGiveUpMs) → damage なしと判断して打ち切り
            Assert.IsFalse(w.OnAfiSample(Ms(6.9f), 5), "7ms 前は待つ");
            Assert.IsTrue(w.OnAfiSample(Ms(7.1f), 5), "7ms 超で打ち切り (deadline 10ms より先に)");
        }

        [Test]
        public void Wait_DeadlineIsAbsoluteCap()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            Assert.IsFalse(w.DeadlineReached(Ms(9.9f)));
            Assert.IsTrue(w.DeadlineReached(Ms(10f)), "BF#1 + 10ms で絶対上限");
        }

        [Test]
        public void Wait_HeavyFrame_DeadlineAlreadyPassed()
        {
            var p = new CefZeroFramePacer();
            p.OnBeginFrame(Bf1, afiNow: 5, inputSentThisFrame: true);
            var w = p.OpenWaitWindow(WaitMs);
            // ゲーム処理が重く recv 到達が BF#1+12ms → 待ちゼロ (自動 cap)
            Assert.IsTrue(w.DeadlineReached(Ms(12f)));
        }
    }
}
