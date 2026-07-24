using System;
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="ScrollSmoother" /> の単体テスト。
    ///     生 wheel delta を残距離に蓄積し、毎フレーム指数追従で均一化排出する
    ///     平滑器が「総量保存・幾何減衰・方向反転・終端スナップ・平滑OFF互換」を
    ///     満たすことを検証する。
    ///     設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
    /// </summary>
    public class ScrollSmootherTests
    {
        private const float Dt60 = 1f / 60f;   // 60fps のフレーム時間
        private const float Tau = 0.045f;      // 既定の時定数 45ms

        // ---- 平滑 OFF (tau <= 0): 従来挙動 (int 切り捨て + 端数繰り越し) ----

        [Test]
        public void TauZero_EmitsImmediately_WithFractionCarry()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 100.7f);
            s.Tick(Dt60, 0f, out _, out var dy);
            Assert.AreEqual(100, dy, "切り捨てで 100 を即時排出");
            // 端数 0.7 が繰り越され、次の 0.5 と合算で 1.2 → 1 排出
            s.AddInput(0f, 0.5f);
            s.Tick(Dt60, 0f, out _, out dy);
            Assert.AreEqual(1, dy, "繰り越し端数 0.7 + 0.5 = 1.2 → 1");
        }

        // ---- 幾何減衰: 大入力が単調減少のグライドに分散される ----

        [Test]
        public void Smoothing_LargeInput_DecaysMonotonically()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1000f);
            s.Tick(Dt60, Tau, out _, out var e1);
            s.Tick(Dt60, Tau, out _, out var e2);
            s.Tick(Dt60, Tau, out _, out var e3);
            Assert.Greater(e1, e2);
            Assert.Greater(e2, e3);
            Assert.Greater(e3, 0);
            // τ=45ms, 60fps では初フレームで残りの約 31% が出る (食いつき確認)
            Assert.That(e1, Is.InRange(280, 340));
        }

        // ---- 総量保存: 小数部ゼロの入力なら排出合計が入力と厳密一致 ----

        [Test]
        public void Smoothing_IntegerInput_ConservesTotal()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1000f);
            var total = 0;
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            Assert.IsFalse(s.IsActive, "200 フレーム以内に排出し切る");
            Assert.AreEqual(1000, total);
        }

        // ---- 方向反転: 逆符号入力で残距離が相殺され、最終総量が一致 ----

        [Test]
        public void Smoothing_Reversal_NetTotalMatches()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 100f);
            var total = 0;
            s.Tick(Dt60, Tau, out _, out var dy);
            total += dy;
            s.AddInput(0f, -200f); // 反転 (残距離 ≈ 69 - 200 = -131)
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out _, out dy);
                total += dy;
            }
            Assert.AreEqual(-100, total, "正味 100 - 200 = -100");
        }

        // ---- 終端スナップ: 入力途絶後、微小残距離は破棄され無限テールにならない ----

        [Test]
        public void Smoothing_TinyResidual_SnapsToZeroAfterStarvation()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 0.4f);
            var total = 0;
            for (var i = 0; i < 3; i++)
            {
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            Assert.AreEqual(0, total, "0.5px 未満は破棄");
            Assert.IsFalse(s.IsActive, "入力途絶 (StarvedTicks 経過) 後にスナップされる");
        }

        // ---- 停滞防止: 入力途絶後、排出0丸め帯域の残距離もテールとして排出し切る ----

        [Test]
        public void Smoothing_MidTailBand_DoesNotStall()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1.4f); // k≈0.31 では 1.4×k≈0.43 → Round=0 の停滞帯域
            var total = 0;
            for (var i = 0; i < 5; i++)
            {
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            Assert.AreEqual(1, total, "停滞せずテールを排出し切る");
            Assert.IsFalse(s.IsActive);
        }

        // ---- 定常サブピクセル入力: スナップ過剰排出 (+25%) も取りこぼし (100%) も起きない ----

        [Test]
        public void Smoothing_SteadySubPixelStream_ConservesTotal()
        {
            var s = new ScrollSmoother();
            var total = 0;
            for (var i = 0; i < 100; i++)
            {
                s.AddInput(0f, 0.8f); // 毎フレーム 0.8px の定常入力 (合計 80px)
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            // 入力停止後のテールを排出し切る
            for (var i = 0; i < 10 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            Assert.IsFalse(s.IsActive);
            Assert.That(total, Is.EqualTo(80).Within(1), "過剰排出なら ~100、取りこぼしなら 0 になる");
        }

        // ---- 出荷値 τ=15ms の動作点でも総量保存が成立する ----

        [Test]
        public void Smoothing_ProductionTau15ms_ConservesTotal()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1000f);
            var total = 0;
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, 0.015f, out _, out var dy);
                total += dy;
            }
            Assert.IsFalse(s.IsActive);
            Assert.AreEqual(1000, total);
        }

        // ---- dt=0 (ポーズ等) では何も排出せず残距離を保持する ----

        [Test]
        public void Smoothing_ZeroDt_EmitsNothingAndKeepsRemainder()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 500f);
            s.Tick(0f, Tau, out _, out var dy);
            Assert.AreEqual(0, dy);
            Assert.IsTrue(s.IsActive, "残距離は保持される");
        }

        // ---- dt 非依存: 同じ実時間なら分割数に依らずほぼ同量を排出 ----

        [Test]
        public void Smoothing_DtInvariance_WithinTolerance()
        {
            var one = new ScrollSmoother();
            one.AddInput(0f, 1000f);
            one.Tick(1f / 30f, Tau, out _, out var single);

            var two = new ScrollSmoother();
            two.AddInput(0f, 1000f);
            two.Tick(Dt60, Tau, out _, out var a);
            two.Tick(Dt60, Tau, out _, out var b);

            Assert.That(single, Is.EqualTo(a + b).Within(2), "int 丸め分の誤差 ±2px まで許容");
        }

        // ---- X 軸も同一機構で処理される ----

        [Test]
        public void Smoothing_XAxis_Works()
        {
            var s = new ScrollSmoother();
            s.AddInput(50f, 0f);
            var total = 0;
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out var dx, out _);
                total += dx;
            }
            Assert.AreEqual(50, total);
        }

        // ---- Reset: 残距離が破棄される ----

        [Test]
        public void Reset_DiscardsRemainder()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 500f);
            s.Reset();
            Assert.IsFalse(s.IsActive);
            s.Tick(Dt60, Tau, out _, out var dy);
            Assert.AreEqual(0, dy);
        }
    }
}
