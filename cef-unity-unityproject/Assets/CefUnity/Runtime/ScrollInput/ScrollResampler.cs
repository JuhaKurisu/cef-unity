using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     生スクロールイベント列を「フレーム時刻」へ再標本化する (Chromium
    ///     LinearResampling 準拠)。イベントから累積位置 P(t) を構築し、毎フレーム
    ///     sampleTime = now − 適応オフセット (イベント間隔 EMA×1.25, 5〜25ms) の P を
    ///     イベント履歴の線形補間 (イベント間) または線形外挿 (最新イベント以後、上限
    ///     ExtrapolationCap) で求め、前回サンプルとの差分を int px で排出する (端数
    ///     繰り越しで総量保存)。momentum 終端では残差を即時排出して停止する。
    ///     オフセットを固定 5ms にすると 60Hz イベント (macOS の momentum は表示レート)
    ///     でイベント無しフレームが外挿上限に当たり hold→ジャンプのビートが出る (実測
    ///     median 0.147)。オフセットを 1 イベント間隔強に適応させ、履歴を 4 点持つ
    ///     ことでサンプルが常に補間帯域に入り、ビートが構造的に消える。
    ///     純 C# (Unity API 非依存)。時刻はイベントと同一クロック (秒) を呼び出し側が渡す。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public sealed class ScrollResampler
    {
        /// <summary>適応サンプルオフセット (now からこの分過去を標本化) の下限/上限 (秒)。</summary>
        public const double MinSampleOffset = 0.005;
        public const double MaxSampleOffset = 0.025;

        /// <summary>最新イベントからの外挿上限 (秒)。超えた分は保持 (オーバーシュート防止)。</summary>
        public const double ExtrapolationCap = 0.008;

        /// <summary>無イベントでジェスチャ終端とみなすグレース (秒)。</summary>
        public const double GraceTimeout = 0.100;

        /// <summary>
        ///     この間隔以下で連続するイベントは同一点にマージする (秒)。ジェスチャ→慣性の
        ///     遷移で macOS は ~0.2ms 差の連続イベント (GestureEnded の dy=0 と
        ///     MomentumBegan) を送るため、そのまま 2 点にすると外挿傾きが発散して
        ///     数千 px のスパイク排出になる (録画リプレイで実測 -1422px, 5099px)。
        /// </summary>
        public const double MergeEpsilon = 0.002;

        // イベント履歴 (時刻昇順、[_count-1] が最新)。オフセットが 1 イベント間隔を
        // 超えても補間できるよう 2 点ではなく 4 点持つ。
        private const int HistoryCap = 4;
        private readonly double[] _t = new double[HistoryCap];
        private readonly double[] _pX = new double[HistoryCap];
        private readonly double[] _pY = new double[HistoryCap];
        private int _count;

        // 前回サンプル位置と、int 排出の端数繰り越し。
        private double _sampX, _sampY;
        private double _fracX, _fracY;

        // momentum Ended/Cancelled 受信済み。次の Tick で残差を排出して停止する。
        private bool _ended;

        // イベント間隔の EMA (秒)。適応サンプルオフセットの元 (初期値 8ms ≒ 120Hz)。
        private double _intervalEma = 0.008;

        // 直近の進行方向 (+1/-1、segment slope の符号)。予測モードの no-backtrack 用。
        private double _lastDirX, _lastDirY;

        /// <summary>追跡中のジェスチャがあるか。</summary>
        public bool IsActive => _count > 0;

        /// <summary>
        ///     予測モード。サンプルを now−MinSampleOffset (5ms) に置き、外挿上限を
        ///     イベント間隔相当まで拡大して遅延を下げる。定常スクロール中は線形予測が
        ///     正確なのでビートは出ない。速度急変時のオーバーシュート巻き戻しは排出せず
        ///     サンプル位置を保持する (no-backtrack。終点誤差は数 px 以内で不可視)。
        ///     false (既定) は補間主体 (遅延 ~1 イベント間隔、アーティファクトなし)。
        /// </summary>
        public bool Predictive { get; set; }

        public void Reset()
        {
            _count = 0;
            _sampX = _sampY = 0;
            _fracX = _fracY = 0;
            _ended = false;
            _intervalEma = 0.008;
            _lastDirX = _lastDirY = 0;
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
                _t[0] = e.Timestamp;
                // 前回サンプル位置から連続に開始する (位置ジャンプ防止)。
                _pX[0] = _sampX + e.DxPx;
                _pY[0] = _sampY + e.DyPx;
                _count = 1;
                return;
            }
            var last = _count - 1;
            if (e.Timestamp <= _t[last] + MergeEpsilon)
            {
                // 近接イベント (同フレーム複数配送・phase 遷移ペア等) は最新点へ合算。
                // 退化セグメント (極小 dt) を作らないことで補間/外挿の傾き発散を防ぐ。
                _pX[last] += e.DxPx;
                _pY[last] += e.DyPx;
                return;
            }
            // 新しい時刻へ進む: イベント間隔 EMA を更新 (適応オフセットの元)。
            // ジェスチャ間の休止 (50ms 超の途切れ) はデバイス周期ではないので除外
            // (混入すると予測モードの外挿上限が膨らみ、再開直後のオーバーシュートが増える)。
            var interval = e.Timestamp - _t[last];
            if (interval < 0.05)
                _intervalEma += (interval - _intervalEma) * 0.2;
            if (_count == HistoryCap)
            {
                // 履歴が満杯: 最古を捨てて左詰め。
                for (var i = 1; i < HistoryCap; i++)
                {
                    _t[i - 1] = _t[i];
                    _pX[i - 1] = _pX[i];
                    _pY[i - 1] = _pY[i];
                }
                _count--;
                last--;
            }
            _t[_count] = e.Timestamp;
            _pX[_count] = _pX[last] + e.DxPx;
            _pY[_count] = _pY[last] + e.DyPx;
            _count++;
        }

        /// <summary>
        ///     残差 (最新イベント位置 − 前回サンプル) を端数バッファへ移し、履歴をクリアする。
        ///     外挿でサンプルが最新位置を追い越していた場合 (残差が直近の進行方向と逆) は
        ///     捨てる — 終端での「巻き戻し」を防ぐ。
        /// </summary>
        private void FlushResidualToFraction()
        {
            var last = _count - 1;
            var rx = _pX[last] - _sampX;
            var ry = _pY[last] - _sampY;
            // 進行方向は永続値 _lastDir で判定する。終端イベントは delta=0 で直近
            // セグメントの傾きが 0 になるため、その場の傾きで判定すると外挿オーバー
            // シュートの負残差がすり抜けて端数に溜まり、次ジェスチャ開始時に
            // 「位置が飛ぶ」(実測バグ)。方向未確定 (0) のときのみ無条件に保存する。
            if (_lastDirX == 0 || rx * _lastDirX >= 0) _fracX += rx;
            if (_lastDirY == 0 || ry * _lastDirY >= 0) _fracY += ry;
            _count = 0;
            _sampX = _sampY = 0;
            _ended = false;
        }

        /// <summary>1 フレーム分の排出量を計算する。now はイベントと同一クロック (秒)。</summary>
        public void Tick(double now, out int dx, out int dy)
        {
            if (_count > 0)
            {
                var last = _count - 1;
                if (_ended || now - _t[last] > GraceTimeout)
                {
                    FlushResidualToFraction();
                }
                else
                {
                    var offset = Predictive
                        ? MinSampleOffset
                        : Math.Min(MaxSampleOffset, Math.Max(MinSampleOffset, _intervalEma * 1.25));
                    var sampleTime = now - offset;
                    double sx, sy;
                    if (_count >= 2 && sampleTime > _t[last])
                    {
                        // 最新イベント以後: 履歴窓全体 (最大4点) の平均速度で外挿 (上限 cap)。
                        // 直近2点だと近接タイムスタンプで傾きが発散する (ノイズ増幅) ため、
                        // 窓の端点間で算出する。
                        var cap = Predictive
                            ? Math.Min(MaxSampleOffset, _intervalEma * 1.25)
                            : ExtrapolationCap;
                        var dt = Math.Min(sampleTime - _t[last], cap);
                        var span = _t[last] - _t[0];
                        sx = _pX[last] + (_pX[last] - _pX[0]) / span * dt;
                        sy = _pY[last] + (_pY[last] - _pY[0]) / span * dt;
                    }
                    else if (_count < 2 || sampleTime >= _t[last])
                    {
                        // 補間に足る2点が無い: 最新位置をそのまま使う (即時排出)。
                        sx = _pX[last];
                        sy = _pY[last];
                    }
                    else if (sampleTime <= _t[0])
                    {
                        sx = _pX[0];
                        sy = _pY[0];
                    }
                    else
                    {
                        // 履歴内: sampleTime を含む区間を探して線形補間 (リサンプリングの本体)。
                        var i = last;
                        while (_t[i - 1] > sampleTime) i--;
                        var a = (sampleTime - _t[i - 1]) / (_t[i] - _t[i - 1]);
                        sx = _pX[i - 1] + (_pX[i] - _pX[i - 1]) * a;
                        sy = _pY[i - 1] + (_pY[i] - _pY[i - 1]) * a;
                    }
                    if (_count >= 2)
                    {
                        // 進行方向を更新 (非ゼロ傾きのみ。終端の delta=0 では保持)。
                        // 予測モードの no-backtrack と、フラッシュ時の巻き戻し防止の両方が使う。
                        var segX = _pX[last] - _pX[last - 1];
                        var segY = _pY[last] - _pY[last - 1];
                        if (segX != 0) _lastDirX = segX > 0 ? 1 : -1;
                        if (segY != 0) _lastDirY = segY > 0 ? 1 : -1;
                        if (Predictive)
                        {
                            // no-backtrack: 逆向きの微小補正 (外挿オーバーシュートの
                            // 巻き戻し) は排出せず位置を保持する。実イベントによる方向
                            // 反転は segment slope が反転するので追従する。
                            if ((sx - _sampX) * _lastDirX < 0) sx = _sampX;
                            if ((sy - _sampY) * _lastDirY < 0) sy = _sampY;
                        }
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
