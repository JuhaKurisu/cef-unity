using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     生スクロールイベント列を「フレーム時刻」へ再標本化する (Chromium
    ///     LinearResampling 準拠)。イベントから累積位置 P(t) を構築し、毎フレーム
    ///     sampleTime = now − SampleOffset の P を直近2イベントの線形補間 (イベント間)
    ///     または線形外挿 (最終イベント以後、上限 ExtrapolationCap) で求め、前回サンプル
    ///     との差分を int px で排出する (端数繰り越しで総量保存)。momentum 終端では残差を
    ///     即時排出して停止する (A案 ScrollSmoother の「停止後の浮遊」が構造的に生じない)。
    ///     純 C# (Unity API 非依存)。時刻はイベントと同一クロック (秒) を呼び出し側が渡す。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public sealed class ScrollResampler
    {
        /// <summary>サンプル時刻のオフセット (秒)。now からこの分だけ過去を標本化する。</summary>
        public const double SampleOffset = 0.005;

        /// <summary>最終イベントからの外挿上限 (秒)。超えた分は保持 (オーバーシュート防止)。</summary>
        public const double ExtrapolationCap = 0.008;

        /// <summary>無イベントでジェスチャ終端とみなすグレース (秒)。</summary>
        public const double GraceTimeout = 0.100;

        // 直近2イベントの (時刻, 累積位置)。_count は保持点数 (0/1/2)。
        private double _t0, _t1;
        private double _p0X, _p0Y, _p1X, _p1Y;
        private int _count;

        // 前回サンプル位置と、int 排出の端数繰り越し。
        private double _sampX, _sampY;
        private double _fracX, _fracY;

        // momentum Ended/Cancelled 受信済み。次の Tick で残差を排出して停止する。
        private bool _ended;

        /// <summary>追跡中のジェスチャがあるか。</summary>
        public bool IsActive => _count > 0;

        public void Reset()
        {
            _count = 0;
            _t0 = _t1 = 0;
            _p0X = _p0Y = _p1X = _p1Y = 0;
            _sampX = _sampY = 0;
            _fracX = _fracY = 0;
            _ended = false;
        }

        /// <summary>イベントを取り込む (delta は view px スケール済みであること)。</summary>
        public void AddEvent(in ScrollInputEvent e)
        {
            if (_ended)
            {
                // 前ジェスチャの終端 Tick を挟まず新ジェスチャが始まった:
                // 残差を端数バッファへ退避してから履歴を作り直す (排出は次の Tick)。
                FlushResidualToFraction();
            }
            Accumulate(e);
            if (e.Phase == ScrollPhase.MomentumEnded || e.Phase == ScrollPhase.Cancelled)
                _ended = true;
        }

        private void Accumulate(in ScrollInputEvent e)
        {
            if (_count == 0)
            {
                _t1 = e.Timestamp;
                // 前回サンプル位置から連続に開始する (位置ジャンプ防止)。
                _p1X = _sampX + e.DxPx;
                _p1Y = _sampY + e.DyPx;
                _count = 1;
                return;
            }
            if (e.Timestamp <= _t1)
            {
                // 同時刻イベント (同フレーム複数イベント等) は最新点へ合算 (0 除算回避)。
                _p1X += e.DxPx;
                _p1Y += e.DyPx;
                return;
            }
            _t0 = _t1;
            _p0X = _p1X;
            _p0Y = _p1Y;
            _t1 = e.Timestamp;
            _p1X += e.DxPx;
            _p1Y += e.DyPx;
            _count = 2;
        }

        /// <summary>
        ///     残差 (最終イベント位置 − 前回サンプル) を端数バッファへ移し、履歴をクリアする。
        ///     外挿でサンプルが最終位置を追い越していた場合 (残差が直近の進行方向と逆) は
        ///     捨てる — 終端での「巻き戻し」を防ぐ。
        /// </summary>
        private void FlushResidualToFraction()
        {
            var rx = _p1X - _sampX;
            var ry = _p1Y - _sampY;
            var dirX = _p1X - _p0X;
            var dirY = _p1Y - _p0Y;
            if (_count < 2 || rx * dirX >= 0) _fracX += rx;
            if (_count < 2 || ry * dirY >= 0) _fracY += ry;
            _count = 0;
            _t0 = _t1 = 0;
            _p0X = _p0Y = _p1X = _p1Y = 0;
            _sampX = _sampY = 0;
            _ended = false;
        }

        /// <summary>1 フレーム分の排出量を計算する。now はイベントと同一クロック (秒)。</summary>
        public void Tick(double now, out int dx, out int dy)
        {
            if (_count > 0)
            {
                if (_ended || now - _t1 > GraceTimeout)
                {
                    FlushResidualToFraction();
                }
                else
                {
                    var sampleTime = now - SampleOffset;
                    double sx, sy;
                    if (_count < 2 || sampleTime >= _t1)
                    {
                        if (_count == 2 && sampleTime > _t1)
                        {
                            // 最終イベント以後: 直近2点の速度で外挿 (上限 cap)。
                            var dt = Math.Min(sampleTime - _t1, ExtrapolationCap);
                            var span = _t1 - _t0;
                            sx = _p1X + (_p1X - _p0X) / span * dt;
                            sy = _p1Y + (_p1Y - _p0Y) / span * dt;
                        }
                        else
                        {
                            // 補間に足る2点が無い: 最新位置をそのまま使う (即時排出)。
                            sx = _p1X;
                            sy = _p1Y;
                        }
                    }
                    else if (sampleTime <= _t0)
                    {
                        sx = _p0X;
                        sy = _p0Y;
                    }
                    else
                    {
                        // イベント間: 線形補間 (リサンプリングの本体)。
                        var a = (sampleTime - _t0) / (_t1 - _t0);
                        sx = _p0X + (_p1X - _p0X) * a;
                        sy = _p0Y + (_p1Y - _p0Y) * a;
                    }
                    _fracX += sx - _sampX;
                    _fracY += sy - _sampY;
                    _sampX = sx;
                    _sampY = sy;
                }
            }
            dx = TakeInt(ref _fracX);
            dy = TakeInt(ref _fracY);
        }

        private static int TakeInt(ref double frac)
        {
            var v = (int)Math.Round(frac);
            frac -= v;
            return v;
        }
    }
}
