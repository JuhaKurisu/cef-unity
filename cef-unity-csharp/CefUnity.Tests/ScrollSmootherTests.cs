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
        private const float DeltaTime60 = 1f / 60f;   // 60fps のフレーム時間
        private const float Tau = 0.045f;             // 既定の時定数 45ms

        // ---- 平滑 OFF (tau <= 0): 従来挙動 (int 切り捨て + 端数繰り越し) ----

        [Test]
        public void TauZero_EmitsImmediately_WithFractionCarry()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 100.7f);
            smoother.Tick(DeltaTime60, 0f, out _, out var deltaY);
            Assert.AreEqual(100, deltaY, "切り捨てで 100 を即時排出");
            // 端数 0.7 が繰り越され、次の 0.5 と合算で 1.2 → 1 排出
            smoother.AddInput(0f, 0.5f);
            smoother.Tick(DeltaTime60, 0f, out _, out deltaY);
            Assert.AreEqual(1, deltaY, "繰り越し端数 0.7 + 0.5 = 1.2 → 1");
        }

        // ---- 幾何減衰: 大入力が単調減少のグライドに分散される ----

        [Test]
        public void Smoothing_LargeInput_DecaysMonotonically()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 1000f);
            smoother.Tick(DeltaTime60, Tau, out _, out var emit1);
            smoother.Tick(DeltaTime60, Tau, out _, out var emit2);
            smoother.Tick(DeltaTime60, Tau, out _, out var emit3);
            Assert.Greater(emit1, emit2);
            Assert.Greater(emit2, emit3);
            Assert.Greater(emit3, 0);
            // τ=45ms, 60fps では初フレームで残りの約 31% が出る (食いつき確認)
            Assert.That(emit1, Is.InRange(280, 340));
        }

        // ---- 総量保存: 小数部ゼロの入力なら排出合計が入力と厳密一致 ----

        [Test]
        public void Smoothing_IntegerInput_ConservesTotal()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 1000f);
            var total = 0;
            for (var frameIndex = 0; frameIndex < 200 && smoother.IsActive; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
                total += deltaY;
            }
            Assert.IsFalse(smoother.IsActive, "200 フレーム以内に排出し切る");
            Assert.AreEqual(1000, total);
        }

        // ---- 方向反転: 逆符号入力で残距離が相殺され、最終総量が一致 ----

        [Test]
        public void Smoothing_Reversal_NetTotalMatches()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 100f);
            var total = 0;
            smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
            total += deltaY;
            smoother.AddInput(0f, -200f); // 反転 (残距離 ≈ 69 - 200 = -131)
            for (var frameIndex = 0; frameIndex < 200 && smoother.IsActive; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out _, out deltaY);
                total += deltaY;
            }
            Assert.AreEqual(-100, total, "正味 100 - 200 = -100");
        }

        // ---- 終端スナップ: 入力途絶後、微小残距離は破棄され無限テールにならない ----

        [Test]
        public void Smoothing_TinyResidual_SnapsToZeroAfterStarvation()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 0.4f);
            var total = 0;
            for (var frameIndex = 0; frameIndex < 3; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
                total += deltaY;
            }
            Assert.AreEqual(0, total, "0.5px 未満は破棄");
            Assert.IsFalse(smoother.IsActive, "入力途絶 (StarvedTicks 経過) 後にスナップされる");
        }

        // ---- 停滞防止: 入力途絶後、排出0丸め帯域の残距離もテールとして排出し切る ----

        [Test]
        public void Smoothing_MidTailBand_DoesNotStall()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 1.4f); // emissionRate≈0.31 では 1.4×emissionRate≈0.43 → Round=0 の停滞帯域
            var total = 0;
            for (var frameIndex = 0; frameIndex < 5; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
                total += deltaY;
            }
            Assert.AreEqual(1, total, "停滞せずテールを排出し切る");
            Assert.IsFalse(smoother.IsActive);
        }

        // ---- 定常サブピクセル入力: スナップ過剰排出 (+25%) も取りこぼし (100%) も起きない ----

        [Test]
        public void Smoothing_SteadySubPixelStream_ConservesTotal()
        {
            var smoother = new ScrollSmoother();
            var total = 0;
            for (var frameIndex = 0; frameIndex < 100; frameIndex++)
            {
                smoother.AddInput(0f, 0.8f); // 毎フレーム 0.8px の定常入力 (合計 80px)
                smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
                total += deltaY;
            }
            // 入力停止後のテールを排出し切る
            for (var frameIndex = 0; frameIndex < 10 && smoother.IsActive; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
                total += deltaY;
            }
            Assert.IsFalse(smoother.IsActive);
            Assert.That(total, Is.EqualTo(80).Within(1), "過剰排出なら ~100、取りこぼしなら 0 になる");
        }

        // ---- 出荷値 τ=15ms の動作点でも総量保存が成立する ----

        [Test]
        public void Smoothing_ProductionTau15ms_ConservesTotal()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 1000f);
            var total = 0;
            for (var frameIndex = 0; frameIndex < 200 && smoother.IsActive; frameIndex++)
            {
                smoother.Tick(DeltaTime60, 0.015f, out _, out var deltaY);
                total += deltaY;
            }
            Assert.IsFalse(smoother.IsActive);
            Assert.AreEqual(1000, total);
        }

        // ---- deltaTime=0 (ポーズ等) では何も排出せず残距離を保持する ----

        [Test]
        public void Smoothing_ZeroDt_EmitsNothingAndKeepsRemainder()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 500f);
            smoother.Tick(0f, Tau, out _, out var deltaY);
            Assert.AreEqual(0, deltaY);
            Assert.IsTrue(smoother.IsActive, "残距離は保持される");
        }

        // ---- deltaTime 非依存: 同じ実時間なら分割数に依らずほぼ同量を排出 ----

        [Test]
        public void Smoothing_DtInvariance_WithinTolerance()
        {
            var one = new ScrollSmoother();
            one.AddInput(0f, 1000f);
            one.Tick(1f / 30f, Tau, out _, out var single);

            var two = new ScrollSmoother();
            two.AddInput(0f, 1000f);
            two.Tick(DeltaTime60, Tau, out _, out var firstEmit);
            two.Tick(DeltaTime60, Tau, out _, out var secondEmit);

            Assert.That(single, Is.EqualTo(firstEmit + secondEmit).Within(2), "int 丸め分の誤差 ±2px まで許容");
        }

        // ---- X 軸も同一機構で処理される ----

        [Test]
        public void Smoothing_XAxis_Works()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(50f, 0f);
            var total = 0;
            for (var frameIndex = 0; frameIndex < 200 && smoother.IsActive; frameIndex++)
            {
                smoother.Tick(DeltaTime60, Tau, out var deltaX, out _);
                total += deltaX;
            }
            Assert.AreEqual(50, total);
        }

        // ---- Reset: 残距離が破棄される ----

        [Test]
        public void Reset_DiscardsRemainder()
        {
            var smoother = new ScrollSmoother();
            smoother.AddInput(0f, 500f);
            smoother.Reset();
            Assert.IsFalse(smoother.IsActive);
            smoother.Tick(DeltaTime60, Tau, out _, out var deltaY);
            Assert.AreEqual(0, deltaY);
        }
    }
}
