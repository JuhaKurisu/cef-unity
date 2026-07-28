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
        private const float BeginFrame1Time = 100f;      // 基準時刻 (秒)
        private const float WaitMilliseconds = 10f;      // _zeroFrameWaitMilliseconds 既定

        private static float AtMilliseconds(float milliseconds) => BeginFrame1Time + milliseconds * 0.001f;

        // ---- プローブ判定 (ShouldSkipAsIdle) ----

        [Test]
        public void Idle_WhenNeverPainted_AndNoInput()
        {
            var pacer = new CefZeroFramePacer();
            Assert.IsTrue(pacer.ShouldSkipAsIdle(inputSentThisFrame: false), "初期状態 (paint 未取得) は静止扱い");
            Assert.IsFalse(pacer.ShouldSkipAsIdle(inputSentThisFrame: true), "入力を送ったフレームは待つ");
        }

        [Test]
        public void ProbeWindow_StaysActiveFor60FramesAfterFreshPaint()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnFreshPaint();
            // 59 回受信なしでも窓内 (framesSince=59 < 60)
            for (var frameIndex = 0; frameIndex < 59; frameIndex++) pacer.OnNoPaint();
            Assert.IsFalse(pacer.ShouldSkipAsIdle(false), "fresh から 59F は窓内 = 待つ");
            pacer.OnNoPaint();
            Assert.IsTrue(pacer.ShouldSkipAsIdle(false), "60F で窓を超え静止扱い");
        }

        // ---- streak 抑止推定 (ShouldSkipAsSuppressed) ----

        [Test]
        public void StreakScore_SuppressesAfter3ConsecutiveFresh()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnFreshPaint();
            pacer.OnFreshPaint();
            Assert.IsFalse(pacer.ShouldSkipAsSuppressed(), "スコア 2 では非抑止");
            pacer.OnFreshPaint();
            Assert.IsTrue(pacer.ShouldSkipAsSuppressed(), "スコア 3 で抑止推定");
        }

        [Test]
        public void StreakScore_HysteresisSurvivesOneMiss()
        {
            var pacer = new CefZeroFramePacer();
            for (var frameIndex = 0; frameIndex < 5; frameIndex++) pacer.OnFreshPaint(); // スコア 5
            pacer.OnNoPaint(); // -2 → 3
            Assert.IsTrue(pacer.ShouldSkipAsSuppressed(), "1 回の取り逃しでは抑止推定を維持 (ヒステリシス)");
            pacer.OnNoPaint(); // -2 → 1
            Assert.IsFalse(pacer.ShouldSkipAsSuppressed(), "2 連続で外れたら解除");
        }

        [Test]
        public void StreakScore_CapsAtMax_ForFastRelease()
        {
            var pacer = new CefZeroFramePacer();
            for (var frameIndex = 0; frameIndex < 100; frameIndex++) pacer.OnFreshPaint(); // 天井 6 で頭打ち
            // 6 → -2×2 = 2 (< 3) で解除される (天井が高いと解除が遅れる)
            pacer.OnNoPaint();
            pacer.OnNoPaint();
            Assert.IsFalse(pacer.ShouldSkipAsSuppressed(), "長時間スクロール後も 2 フレームで解除");
        }

        // ---- 連続入力スキップ ----

        [Test]
        public void SustainedInput_SkipsAfter3ConsecutiveInputFrames()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, 0, inputSentThisFrame: true);
            pacer.OnBeginFrame(BeginFrame1Time, 0, inputSentThisFrame: true);
            Assert.IsFalse(pacer.ShouldSkipAsSuppressed(), "連続 2 フレームでは待つ (単発入力は 0F を取る)");
            pacer.OnBeginFrame(BeginFrame1Time, 0, inputSentThisFrame: true);
            Assert.IsTrue(pacer.ShouldSkipAsSuppressed(), "連続 3 フレームでスキップ");
            pacer.OnBeginFrame(BeginFrame1Time, 0, inputSentThisFrame: false);
            Assert.IsFalse(pacer.ShouldSkipAsSuppressed(), "入力が途切れたら即リセット");
        }

        // ---- busy-wait 窓 (ZeroFrameWaitWindow) ----

        [Test]
        public void Wait_FreshPaintAfterMinDelay_EndsWait()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            // 4.5ms (FreshPaintMinDelayMilliseconds) 以降の増分 = fresh (#B) → 即終了
            Assert.IsFalse(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(2f), 5), "増分なしは継続");
            Assert.IsTrue(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(5f), 6), "freshMinTime 後の増分で待ち終了");
        }

        [Test]
        public void Wait_StalePaintBeforeMinDelay_IsSkippedThenFreshTaken()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            // 4.5ms より前の増分 = BF#1 由来 stale (#A) → 読み捨てて継続
            Assert.IsFalse(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(2f), 6), "stale (#A) は読み捨てて待ち続行");
            // その後 fresh (#B) が来たら終了
            Assert.IsTrue(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(6f), 7), "fresh (#B) で終了");
        }

        [Test]
        public void Wait_EarlyPaintWithoutFresh_AdoptsAtEarlyAdoptTime()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            Assert.IsFalse(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(2f), 6), "stale (#A) 読み捨て");
            // #B が来ないまま 7.5ms (EarlyPaintAdoptMilliseconds) 到達 → #A を採用して終了
            Assert.IsFalse(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(7f), 6), "earlyAdopt 前は粘る");
            Assert.IsTrue(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(7.6f), 6), "earlyAdopt で #A 採用");
        }

        [Test]
        public void Wait_NoDamage_GivesUpAt7ms()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            // 増分ゼロのまま 7ms (NoDamageGiveUpMilliseconds) → damage なしと判断して打ち切り
            Assert.IsFalse(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(6.9f), 5), "7ms 前は待つ");
            Assert.IsTrue(waitWindow.OnAcceleratedFrameIdSample(AtMilliseconds(7.1f), 5), "7ms 超で打ち切り (deadline 10ms より先に)");
        }

        [Test]
        public void Wait_DeadlineIsAbsoluteCap()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            Assert.IsFalse(waitWindow.DeadlineReached(AtMilliseconds(9.9f)));
            Assert.IsTrue(waitWindow.DeadlineReached(AtMilliseconds(10f)), "BF#1 + 10ms で絶対上限");
        }

        [Test]
        public void Wait_HeavyFrame_DeadlineAlreadyPassed()
        {
            var pacer = new CefZeroFramePacer();
            pacer.OnBeginFrame(BeginFrame1Time, acceleratedFrameIdNow: 5, inputSentThisFrame: true);
            var waitWindow = pacer.OpenWaitWindow(WaitMilliseconds);
            // ゲーム処理が重く recv 到達が BF#1+12ms → 待ちゼロ (自動 cap)
            Assert.IsTrue(waitWindow.DeadlineReached(AtMilliseconds(12f)));
        }
    }
}
