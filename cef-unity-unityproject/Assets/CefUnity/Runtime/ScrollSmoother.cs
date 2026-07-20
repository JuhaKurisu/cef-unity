using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     スクロール入力の平滑器 (指数追従)。生の wheel delta を「未送信の残距離」に
    ///     蓄積し、毎フレーム残距離の一定割合 (k = 1 - exp(-dt/τ)) を int px で排出する。
    ///     per-frame のスクロール送信量が均一化され、トラックパッド生 delta の
    ///     ジッター/フリック巨大単発 (診断: CoV 0.82, 最大 1138px/frame) が
    ///     幾何減衰のグライドに変わる。Chrome の入力リサンプリング (層2) 相当の
    ///     クライアント側実装。Unity API 非依存 (EditMode テスト可能)。
    ///     設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
    /// </summary>
    public sealed class ScrollSmoother
    {
        private float _remainX;
        private float _remainY;

        /// <summary>残距離が残っているか (排出継続の判定用)。</summary>
        public bool IsActive => _remainX != 0f || _remainY != 0f;

        /// <summary>入力 delta (px) を残距離に加算する。方向反転は符号の相殺で自然に処理。</summary>
        public void AddInput(float dxPx, float dyPx)
        {
            _remainX += dxPx;
            _remainY += dyPx;
        }

        /// <summary>残距離を破棄する (ナビゲーション時など)。</summary>
        public void Reset()
        {
            _remainX = 0f;
            _remainY = 0f;
        }

        /// <summary>
        ///     dt 秒経過分の排出量を計算する。tau &lt;= 0 は平滑 OFF
        ///     (従来挙動: int 切り捨て + 端数繰り越しで即時全量排出)。
        /// </summary>
        public void Tick(float dt, float tau, out int dx, out int dy)
        {
            // k < 0 を「平滑 OFF」の番兵に使う (排出率としての k は常に [0,1))。
            var k = tau <= 0f ? -1f : 1f - (float)Math.Exp(-dt / tau);
            dx = TickAxis(ref _remainX, k);
            dy = TickAxis(ref _remainY, k);
        }

        private static int TickAxis(ref float remain, float k)
        {
            if (remain == 0f) return 0;
            int emit;
            if (k < 0f)
            {
                // 平滑 OFF: 旧 _wheelAccum と同じ「切り捨て + 端数繰り越し」。
                emit = (int)remain;
                remain -= emit;
                return emit;
            }
            if (Math.Abs(remain) <= 1f)
            {
                // 終端スナップ: 無限テール防止。0.5px 未満の端数は破棄 (許容損失)。
                emit = (int)Math.Round(remain);
                remain = 0f;
                return emit;
            }
            emit = (int)Math.Round(remain * k);
            if (emit == 0 && k > 0f)
            {
                // 排出が 0 に丸まる帯域 (|remain| < 0.5/k) で停滞しないよう、
                // 残距離をテールとして排出し切る (スタック防止)。dt=0 (k=0) は除外。
                emit = (int)Math.Round(remain);
                remain = 0f;
                return emit;
            }
            remain -= emit; // int で減算するので端数は残距離に残る (総量保存)
            return emit;
        }
    }
}
