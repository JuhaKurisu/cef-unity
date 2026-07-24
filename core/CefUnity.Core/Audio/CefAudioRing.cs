using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     CEF (producer, 実時間 srcRate) → Unity 出力 (consumer, outRate) の
    ///     非同期サンプルレート変換つきリングバッファ。
    ///     <para>
    ///     producer と consumer はクロックが独立しておりレートがわずかにずれる。
    ///     固定レートで読むと滞留量が一方向にドリフトし、いずれ
    ///     アンダーラン (無音) かオーバーフロー (破棄) で音がぶつ切りになる。
    ///     そこで本クラスは滞留量の誤差に応じて消費レートを ±<see cref="_maxRateAdjust" />
    ///     だけ滑らかに操作し (steering)、線形補間で出力することで
    ///     クリックもアンダーランも出さずに目標滞留量へ収束させる
    ///     (async sample rate converter)。
    ///     </para>
    ///     <para>
    ///     スレッド: producer (メインスレッド) が <see cref="Write" />、
    ///     consumer (オーディオスレッド) が <see cref="Read" /> を呼ぶ前提で内部ロックする。
    ///     UnityEngine 非依存 (System のみ) なので単体テスト可能。
    ///     </para>
    /// </summary>
    public sealed class CefAudioRing
    {
        private readonly float[] _buf; // interleaved
        private readonly int _capFrames;
        private readonly int _channels;
        private readonly int _targetFrames;
        private readonly double _maxRateAdjust; // 消費レート操作の上限 (例 0.01 = ±1%)
        private readonly object _lock = new object();

        private long _writeFrame;  // 累積書き込みフレーム数 (producer)
        private double _readFrame; // 小数フレーム位置 (consumer)。常に _writeFrame 以下。
        private bool _primed;      // 初回に目標滞留量へ達したか。達するまでは無音を出す。

        public CefAudioRing(int capacityFrames, int channels, int targetFrames, double maxRateAdjust = 0.01)
        {
            if (capacityFrames < 2) capacityFrames = 2;
            if (channels < 1) channels = 1;
            _capFrames = capacityFrames;
            _channels = channels;
            _targetFrames = Math.Min(Math.Max(1, targetFrames), capacityFrames - 1);
            _maxRateAdjust = maxRateAdjust;
            _buf = new float[capacityFrames * channels];
        }

        // ----- 診断カウンタ -----

        /// <summary>データ不足で無音を出した出力フレーム数 (累積)。&gt;0 ならアンダーラン発生。</summary>
        public long UnderrunFrames { get; private set; }

        /// <summary>容量超過で破棄した入力フレーム数 (累積)。&gt;0 ならオーバーフロー発生。</summary>
        public long OverflowDropFrames { get; private set; }

        /// <summary>producer が書いた累積フレーム数。</summary>
        public long ProducedFrames { get; private set; }

        /// <summary>consumer が出力した累積フレーム数。</summary>
        public long ConsumedFrames { get; private set; }

        public int Channels => _channels;
        public int TargetFrames => _targetFrames;
        public int CapacityFrames => _capFrames;

        /// <summary>現在の滞留フレーム数 (producer が書いて consumer がまだ消費していない量)。</summary>
        public double OccupancyFrames
        {
            get
            {
                lock (_lock) { return _writeFrame - _readFrame; }
            }
        }

        /// <summary>
        ///     producer: interleaved サンプル <paramref name="src" />[offsetSamples ..
        ///     +frameCount*channels] を書く。容量を超える場合は最古フレームを捨てる
        ///     (バックストップ。steering が効いていれば通常起きない)。
        /// </summary>
        public void Write(float[] src, int offsetSamples, int frameCount)
        {
            if (frameCount <= 0) return;
            lock (_lock)
            {
                // パケット自体が容量を超える: 最新側だけ残す。
                if (frameCount > _capFrames)
                {
                    int skip = frameCount - _capFrames;
                    offsetSamples += skip * _channels;
                    OverflowDropFrames += skip;
                    frameCount = _capFrames;
                }

                // 空き不足: 最古を捨てる = read 位置を前進。
                long occ = _writeFrame - (long)Math.Floor(_readFrame);
                long free = _capFrames - occ;
                if (frameCount > free)
                {
                    long drop = frameCount - free;
                    _readFrame += drop;
                    OverflowDropFrames += drop;
                }

                for (int f = 0; f < frameCount; f++)
                {
                    int dstBase = (int)(_writeFrame % _capFrames) * _channels;
                    int srcBase = offsetSamples + f * _channels;
                    for (int c = 0; c < _channels; c++)
                        _buf[dstBase + c] = src[srcBase + c];
                    _writeFrame++;
                }

                ProducedFrames += frameCount;
            }
        }

        /// <summary>
        ///     consumer: <paramref name="dst" /> を <paramref name="frameCount" /> フレーム分
        ///     (interleaved) 埋める。<paramref name="baseStep" /> = srcRate/outRate
        ///     (出力1フレームあたり進める src フレーム数)。滞留量が目標から外れていれば
        ///     step を ±<see cref="_maxRateAdjust" /> だけ操作して収束させる。
        ///     データ不足時は無音で埋め <see cref="UnderrunFrames" /> を加算する。
        /// </summary>
        public void Read(float[] dst, int frameCount, double baseStep)
        {
            if (frameCount <= 0) return;
            lock (_lock)
            {
                for (int f = 0; f < frameCount; f++)
                {
                    int ob = f * _channels;
                    double occ = _writeFrame - _readFrame;

                    // 初回プライミング: 目標滞留量に達するまでは無音 (read を進めない)。
                    // steering で徐々に溜める手もあるが、開始直後のピッチ揺れを避けるため
                    // クリーンに目標まで貯めてから再生開始する。
                    if (!_primed)
                    {
                        if (occ < _targetFrames)
                        {
                            for (int c = 0; c < _channels; c++) dst[ob + c] = 0f;
                            UnderrunFrames++;
                            continue;
                        }

                        _primed = true;
                    }

                    // 線形補間には floor と floor+1 の 2 フレームが要る。
                    if (occ < 2.0)
                    {
                        for (int c = 0; c < _channels; c++) dst[ob + c] = 0f;
                        UnderrunFrames++;
                        continue;
                    }

                    long i0 = (long)Math.Floor(_readFrame);
                    float frac = (float)(_readFrame - i0);
                    int b0 = (int)(i0 % _capFrames) * _channels;
                    int b1 = (int)((i0 + 1) % _capFrames) * _channels;
                    for (int c = 0; c < _channels; c++)
                    {
                        float s0 = _buf[b0 + c];
                        float s1 = _buf[b1 + c];
                        dst[ob + c] = s0 + (s1 - s0) * frac;
                    }

                    // レート操作: 滞留量誤差を [-1,1] に正規化し ±maxRateAdjust を掛ける。
                    // occ > target → step を大きく (速く消費) して滞留を減らす。逆も同様。
                    double err = (occ - _targetFrames) / _targetFrames;
                    if (err > 1.0) err = 1.0;
                    else if (err < -1.0) err = -1.0;
                    double step = baseStep * (1.0 + _maxRateAdjust * err);

                    // 補間に floor+1 が要るので利用可能量を食い尽くさないようガード。
                    double maxAdvance = occ - 1.0;
                    if (step > maxAdvance) step = maxAdvance;
                    if (step < 0.0) step = 0.0;
                    _readFrame += step;
                    ConsumedFrames++;
                }
            }
        }
    }
}
