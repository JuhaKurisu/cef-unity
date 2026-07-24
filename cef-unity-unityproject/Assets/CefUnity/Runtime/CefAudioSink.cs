using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     <see cref="CefAudioRing" /> から音声を取り出して Unity のオーディオ出力ミックスへ
    ///     流す音声シンク (consumer)。<see cref="OnAudioFilterRead" /> は DSP ブロックごとに
    ///     一定ペースで呼ばれるため、ストリーミング <see cref="AudioClip" /> の PCMReaderCallback
    ///     のような先読みの波が無く、消費レートが安定する。
    ///     <para>
    ///     <see cref="OnAudioFilterRead" /> は同一 GameObject に AudioListener と AudioSource が
    ///     同居するとどちらにバインドされるか非決定的になり (Unity が警告)、動作が不安定になる。
    ///     これを避けるため本コンポーネントは <b>AudioListener を持たない専用 GameObject 上に
    ///     AudioSource とだけ同居</b>させて使う。<see cref="CefAudioOutput" /> が子オブジェクトと
    ///     して生成・設定する。
    ///     </para>
    /// </summary>
    [RequireComponent(typeof(AudioSource))]
    public sealed class CefAudioSink : MonoBehaviour
    {
        private CefAudioRing _ring;
        private double _baseStep;  // sourceRate / outputRate
        private int _sourceChannels;
        private float[] _scratch;  // src チャネル interleaved (オーディオスレッドでの確保を避ける)
        private AudioSource _source;

        // 計装 (オーディオスレッド ⇄ メインスレッド)。
        private readonly object _statisticsLock = new object();
        private int _calls;
        private int _frames;
        private int _maxBlock;
        private double _outSumSquared;
        private long _outSamples;

        /// <summary>
        ///     リング・レート・チャネルを設定し、無音キャリアクリップを再生して
        ///     <see cref="OnAudioFilterRead" /> の DSP コールバックを駆動する。再呼び出し可。
        /// </summary>
        public void Configure(CefAudioRing ring, double baseStep, int sourceChannels, int outputRate, int maxFrames)
        {
            _ring = ring;
            _baseStep = baseStep;
            _sourceChannels = Mathf.Max(1, sourceChannels);
            if (_scratch == null || _scratch.Length < maxFrames * _sourceChannels)
                _scratch = new float[maxFrames * _sourceChannels];

            if (_source == null) _source = GetComponent<AudioSource>();
            _source.playOnAwake = false;
            _source.loop = true;
            _source.spatialBlend = 0f; // 2D (UI ブラウザ音声を想定)

            // OnAudioFilterRead を駆動するための無音ループクリップ。中身は使わない (0 のまま)。
            if (_source.clip == null)
            {
                int length = Mathf.Max(256, outputRate / 10);
                _source.clip = AudioClip.Create("CefAudioCarrier", length, _sourceChannels, outputRate, false);
            }

            if (!_source.isPlaying) _source.Play();
        }

        /// <summary>出力を止める (AudioSource を停止)。</summary>
        public void StopOutput()
        {
            if (_source != null) _source.Stop();
        }

        /// <summary>consumer 計装のスナップショットを取得してリセットする (メインスレッドから呼ぶ)。</summary>
        public void SnapshotStatistics(out int calls, out int frames, out int maxBlock, out double outSumSquared, out long outSamples)
        {
            lock (_statisticsLock)
            {
                calls = _calls;
                frames = _frames;
                maxBlock = _maxBlock;
                outSumSquared = _outSumSquared;
                outSamples = _outSamples;
                _calls = 0;
                _frames = 0;
                _maxBlock = 0;
                _outSumSquared = 0.0;
                _outSamples = 0;
            }
        }

        // オーディオスレッド (DSP): リングからレート変換した音声を最終ミックスへ加算する。
        // data は出力スピーカーの interleaved (長さ = フレーム数 * channels)。
        private void OnAudioFilterRead(float[] data, int channels)
        {
            var ring = _ring;
            var scratch = _scratch;
            if (ring == null || _sourceChannels <= 0 || scratch == null || channels <= 0)
                return; // 何もしない = 無音 (キャリアの 0) のまま

            int frames = data.Length / channels;
            int need = frames * _sourceChannels;
            if (need > scratch.Length) return; // 想定外の巨大ブロックは安全側でスキップ

            // src チャネルで補間しつつ取り出す。
            ring.Read(scratch, frames, _baseStep);

            // 出力検証用の RMS は per-sample コストになるため診断ログ有効時のみ集計する。
            bool log = CefLog.Enabled;
            double sumSquared = 0.0;
            if (log)
                for (int sampleIndex = 0; sampleIndex < need; sampleIndex++)
                {
                    float sample = scratch[sampleIndex];
                    sumSquared += (double)sample * sample;
                }

            // 最終ミックスへ加算。src と出力のチャネル数が同じなら直接、違えば写像する。
            if (channels == _sourceChannels)
            {
                int sampleCount = frames * channels;
                for (int sampleIndex = 0; sampleIndex < sampleCount; sampleIndex++) data[sampleIndex] += scratch[sampleIndex];
            }
            else
            {
                for (int frameIndex = 0; frameIndex < frames; frameIndex++)
                for (int channelIndex = 0; channelIndex < channels; channelIndex++)
                    data[frameIndex * channels + channelIndex] += scratch[frameIndex * _sourceChannels + channelIndex % _sourceChannels];
            }

            if (log)
                lock (_statisticsLock)
                {
                    _calls++;
                    _frames += frames;
                    if (frames > _maxBlock) _maxBlock = frames;
                    _outSumSquared += sumSquared;
                    _outSamples += need;
                }
        }
    }
}
