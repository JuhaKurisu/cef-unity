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

        /// <summary>
        ///     慣性 (momentum) 中のイベント欠落を橋渡しする外挿上限 (秒、予測モードのみ)。
        ///     メインスレッドのブロックで OS がイベントをコアレッシングし、慣性中に最大
        ///     66ms (4F) の欠落 + 欠落明けの溜め分巨大イベントが実測されている
        ///     (2026-07-23, test-results/scroll-drought-2026-07-23/)。慣性中は指が離れて
        ///     おり、イベントは OS 減衰カーブの機械出力なので、長い外挿でも「指を止めた
        ///     のに滑る」幽霊スクロールにはならない (指接触中はこの橋渡しを行わない —
        ///     Chromium が外挿を 20ms で打ち切るのと同じ判断)。GraceTimeout (100ms) が
        ///     最終的な終端検出として機能するため、それ未満に設定すること。
        /// </summary>
        public const double MomentumBridgeCap = 0.070;

        /// <summary>無イベントでジェスチャ終端とみなすグレース (秒)。</summary>
        public const double GraceTimeout = 0.100;

        /// <summary>
        ///     サンプル位置の 1 tick 移動量の上限 = 観測速度 (履歴窓平均) × この係数。
        ///     ジェスチャ開始の加速中、サンプルがイベント位相の揺れで「1 拍遅れ →
        ///     溜めた不足分を一括放出」する過渡 (実録画 2026-07-23 build: 入力 -34/F に
        ///     対し排出 -58/F の倍返し → 低速ストローク連打でガタつき) を、不足分を
        ///     数フレームに分けた追いつきに均す。定常状態は advance = v×dt でこの上限に
        ///     当たらない。総量は保存される (終端フラッシュは対象外の即時排出)。
        /// </summary>
        public const double CatchUpHeadroom = 1.2;

        /// <summary>
        ///     この間隔以下で連続するイベントは同一点にマージする (秒)。ジェスチャ→慣性の
        ///     遷移で macOS は ~0.2ms 差の連続イベント (GestureEnded の dy=0 と
        ///     MomentumBegan) を送るため、そのまま 2 点にすると外挿傾きが発散して
        ///     数千 px のスパイク排出になる (録画リプレイで実測 -1422px, 5099px)。
        /// </summary>
        public const double MergeEpsilon = 0.002;

        // イベント履歴 (時刻昇順、[_count-1] が最新)。オフセットが 1 イベント間隔を
        // 超えても補間できるよう 2 点ではなく 4 点持つ。
        private const int HistoryCapacity = 4;
        private readonly double[] _times = new double[HistoryCapacity];
        private readonly double[] _positionsX = new double[HistoryCapacity];
        private readonly double[] _positionsY = new double[HistoryCapacity];
        private int _count;

        // 前回サンプル位置と、int 排出の端数繰り越し。
        private double _sampleX, _sampleY;
        private double _fractionX, _fractionY;

        // momentum Ended/Cancelled 受信済み。次の Tick で残差を排出して停止する。
        private bool _ended;

        // イベント間隔の EMA (秒)。適応サンプルオフセットの元 (初期値 8ms ≒ 120Hz)。
        private double _intervalExponentialMovingAverage = 0.008;

        // 直近の進行方向 (+1/-1、segment slope の符号)。予測モードの no-backtrack 用。
        private double _lastDirectionX, _lastDirectionY;

        // 直近イベントが慣性フェーズ (MomentumBegan/Changed) か。欠落橋渡しの適用条件。
        private bool _momentum;

        // 前回 Tick の時刻 (追いつき上限の dt 算出用)。0 = 未 Tick。
        private double _lastTickTime;

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
            _sampleX = _sampleY = 0;
            _fractionX = _fractionY = 0;
            _ended = false;
            _intervalExponentialMovingAverage = 0.008;
            _lastDirectionX = _lastDirectionY = 0;
            _momentum = false;
            _lastTickTime = 0;
        }

        /// <summary>イベントを取り込む (delta は view px スケール済みであること)。</summary>
        public void AddEvent(in ScrollInputEvent inputEvent)
        {
            if (_ended)
            {
                // 前ジェスチャの終端 Tick を挟まず新ジェスチャが始まった:
                // 残差を端数バッファへ退避してから履歴を作り直す (排出は次の Tick)。
                FlushResidualToFraction();
            }
            Accumulate(inputEvent);
            _momentum = inputEvent.Phase == ScrollPhase.MomentumBegan || inputEvent.Phase == ScrollPhase.MomentumChanged;
            if (inputEvent.Phase == ScrollPhase.MomentumEnded || inputEvent.Phase == ScrollPhase.Cancelled)
                _ended = true;
        }

        private void Accumulate(in ScrollInputEvent inputEvent)
        {
            if (_count == 0)
            {
                _times[0] = inputEvent.Timestamp;
                // 前回サンプル位置から連続に開始する (位置ジャンプ防止)。
                _positionsX[0] = _sampleX + inputEvent.DeltaXPixels;
                _positionsY[0] = _sampleY + inputEvent.DeltaYPixels;
                _count = 1;
                return;
            }
            var last = _count - 1;
            if (inputEvent.Timestamp <= _times[last] + MergeEpsilon)
            {
                // 近接イベント (同フレーム複数配送・phase 遷移ペア等) は最新点へ合算。
                // 退化セグメント (極小 dt) を作らないことで補間/外挿の傾き発散を防ぐ。
                _positionsX[last] += inputEvent.DeltaXPixels;
                _positionsY[last] += inputEvent.DeltaYPixels;
                return;
            }
            // 新しい時刻へ進む: イベント間隔 EMA を更新 (適応オフセットの元)。
            // ジェスチャ間の休止 (50ms 超の途切れ) はデバイス周期ではないので除外
            // (混入すると予測モードの外挿上限が膨らみ、再開直後のオーバーシュートが増える)。
            var interval = inputEvent.Timestamp - _times[last];
            if (interval < 0.05)
                _intervalExponentialMovingAverage += (interval - _intervalExponentialMovingAverage) * 0.2;
            if (_count == HistoryCapacity)
            {
                // 履歴が満杯: 最古を捨てて左詰め。
                for (var index = 1; index < HistoryCapacity; index++)
                {
                    _times[index - 1] = _times[index];
                    _positionsX[index - 1] = _positionsX[index];
                    _positionsY[index - 1] = _positionsY[index];
                }
                _count--;
                last--;
            }
            _times[_count] = inputEvent.Timestamp;
            _positionsX[_count] = _positionsX[last] + inputEvent.DeltaXPixels;
            _positionsY[_count] = _positionsY[last] + inputEvent.DeltaYPixels;
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
            var residualX = _positionsX[last] - _sampleX;
            var residualY = _positionsY[last] - _sampleY;
            // 進行方向は永続値 _lastDirection で判定する。終端イベントは delta=0 で直近
            // セグメントの傾きが 0 になるため、その場の傾きで判定すると外挿オーバー
            // シュートの負残差がすり抜けて端数に溜まり、次ジェスチャ開始時に
            // 「位置が飛ぶ」(実測バグ)。方向未確定 (0) のときのみ無条件に保存する。
            if (_lastDirectionX == 0 || residualX * _lastDirectionX >= 0) _fractionX += residualX;
            if (_lastDirectionY == 0 || residualY * _lastDirectionY >= 0) _fractionY += residualY;
            _count = 0;
            _sampleX = _sampleY = 0;
            _ended = false;
        }

        /// <summary>1 フレーム分の排出量を計算する。now はイベントと同一クロック (秒)。</summary>
        public void Tick(double now, out int deltaX, out int deltaY)
        {
            if (_count > 0)
            {
                var last = _count - 1;
                if (_ended || now - _times[last] > GraceTimeout)
                {
                    FlushResidualToFraction();
                }
                else
                {
                    var offset = Predictive
                        ? MinSampleOffset
                        : Math.Min(MaxSampleOffset, Math.Max(MinSampleOffset, _intervalExponentialMovingAverage * 1.25));
                    var sampleTime = now - offset;
                    double sampleX, sampleY;
                    if (_count >= 2 && sampleTime > _times[last])
                    {
                        // 最新イベント以後: 履歴窓全体 (最大4点) の平均速度で外挿 (上限 cap)。
                        // 直近2点だと近接タイムスタンプで傾きが発散する (ノイズ増幅) ため、
                        // 窓の端点間で算出する。慣性中 (予測モード) はイベント欠落の
                        // 橋渡しとして上限を MomentumBridgeCap まで拡大する (定義箇所の
                        // コメント参照。通常運転では sampleTime−times[last] ≦ ~12ms なので
                        // この拡大は欠落フレームにしか効かない)。
                        var extrapolationLimit = Predictive
                            ? (_momentum
                                ? MomentumBridgeCap
                                : Math.Min(MaxSampleOffset, _intervalExponentialMovingAverage * 1.25))
                            : ExtrapolationCap;
                        var deltaTime = Math.Min(sampleTime - _times[last], extrapolationLimit);
                        var span = _times[last] - _times[0];
                        sampleX = _positionsX[last] + (_positionsX[last] - _positionsX[0]) / span * deltaTime;
                        sampleY = _positionsY[last] + (_positionsY[last] - _positionsY[0]) / span * deltaTime;
                    }
                    else if (_count < 2 || sampleTime >= _times[last])
                    {
                        // 補間に足る2点が無い: 最新位置をそのまま使う (即時排出)。
                        sampleX = _positionsX[last];
                        sampleY = _positionsY[last];
                    }
                    else if (sampleTime <= _times[0])
                    {
                        sampleX = _positionsX[0];
                        sampleY = _positionsY[0];
                    }
                    else
                    {
                        // 履歴内: sampleTime を含む区間を探して線形補間 (リサンプリングの本体)。
                        var index = last;
                        while (_times[index - 1] > sampleTime) index--;
                        var interpolationFactor = (sampleTime - _times[index - 1]) / (_times[index] - _times[index - 1]);
                        sampleX = _positionsX[index - 1] + (_positionsX[index] - _positionsX[index - 1]) * interpolationFactor;
                        sampleY = _positionsY[index - 1] + (_positionsY[index] - _positionsY[index - 1]) * interpolationFactor;
                    }
                    // 追いつき上限 (CatchUpHeadroom の定義コメント参照): 観測速度を超える
                    // 一括放出を数フレームに分散する。定常状態では上限に当たらない。
                    if (_count >= 2 && _lastTickTime > 0)
                    {
                        var tickDeltaTime = Math.Min(0.1, Math.Max(0.001, now - _lastTickTime));
                        var windowSpan = _times[last] - _times[0];
                        if (windowSpan > 0)
                        {
                            var maxX = Math.Abs(_positionsX[last] - _positionsX[0]) / windowSpan * tickDeltaTime * CatchUpHeadroom;
                            var maxY = Math.Abs(_positionsY[last] - _positionsY[0]) / windowSpan * tickDeltaTime * CatchUpHeadroom;
                            sampleX = Math.Clamp(sampleX, _sampleX - maxX, _sampleX + maxX);
                            sampleY = Math.Clamp(sampleY, _sampleY - maxY, _sampleY + maxY);
                        }
                    }
                    if (_count >= 2)
                    {
                        // 進行方向を更新 (非ゼロ傾きのみ。終端の delta=0 では保持)。
                        // 予測モードの no-backtrack と、フラッシュ時の巻き戻し防止の両方が使う。
                        var segmentX = _positionsX[last] - _positionsX[last - 1];
                        var segmentY = _positionsY[last] - _positionsY[last - 1];
                        if (segmentX != 0) _lastDirectionX = segmentX > 0 ? 1 : -1;
                        if (segmentY != 0) _lastDirectionY = segmentY > 0 ? 1 : -1;
                        if (Predictive)
                        {
                            // no-backtrack: 逆向きの微小補正 (外挿オーバーシュートの
                            // 巻き戻し) は排出せず位置を保持する。実イベントによる方向
                            // 反転は segment slope が反転するので追従する。
                            if ((sampleX - _sampleX) * _lastDirectionX < 0) sampleX = _sampleX;
                            if ((sampleY - _sampleY) * _lastDirectionY < 0) sampleY = _sampleY;
                        }
                    }
                    _fractionX += sampleX - _sampleX;
                    _fractionY += sampleY - _sampleY;
                    _sampleX = sampleX;
                    _sampleY = sampleY;
                }
            }
            _lastTickTime = now;
            deltaX = TakeInt(ref _fractionX);
            deltaY = TakeInt(ref _fractionY);
        }

        private static int TakeInt(ref double fraction)
        {
            var value = (int)Math.Round(fraction);
            fraction -= value;
            return value;
        }
    }
}
