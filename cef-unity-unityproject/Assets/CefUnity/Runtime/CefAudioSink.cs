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
        private double _baseStep;  // srcRate / outRate
        private int _srcChannels;
        private float[] _scratch;  // src チャネル interleaved (オーディオスレッドでの確保を避ける)
        private AudioSource _source;

        // 計装 (オーディオスレッド ⇄ メインスレッド)。
        private readonly object _statsLock = new object();
        private int _calls;
        private int _frames;
        private int _maxBlock;
        private double _outSumSq;
        private long _outSamples;

        /// <summary>
        ///     リング・レート・チャネルを設定し、無音キャリアクリップを再生して
        ///     <see cref="OnAudioFilterRead" /> の DSP コールバックを駆動する。再呼び出し可。
        /// </summary>
        public void Configure(CefAudioRing ring, double baseStep, int srcChannels, int outRate, int maxFrames)
        {
            _ring = ring;
            _baseStep = baseStep;
            _srcChannels = Mathf.Max(1, srcChannels);
            if (_scratch == null || _scratch.Length < maxFrames * _srcChannels)
                _scratch = new float[maxFrames * _srcChannels];

            if (_source == null) _source = GetComponent<AudioSource>();
            _source.playOnAwake = false;
            _source.loop = true;
            _source.spatialBlend = 0f; // 2D (UI ブラウザ音声を想定)

            // OnAudioFilterRead を駆動するための無音ループクリップ。中身は使わない (0 のまま)。
            if (_source.clip == null)
            {
                int len = Mathf.Max(256, outRate / 10);
                _source.clip = AudioClip.Create("CefAudioCarrier", len, _srcChannels, outRate, false);
            }

            if (!_source.isPlaying) _source.Play();
        }

        /// <summary>出力を止める (AudioSource を停止)。</summary>
        public void StopOutput()
        {
            if (_source != null) _source.Stop();
        }

        /// <summary>consumer 計装のスナップショットを取得してリセットする (メインスレッドから呼ぶ)。</summary>
        public void SnapshotStats(out int calls, out int frames, out int maxBlock, out double outSumSq, out long outSamples)
        {
            lock (_statsLock)
            {
                calls = _calls;
                frames = _frames;
                maxBlock = _maxBlock;
                outSumSq = _outSumSq;
                outSamples = _outSamples;
                _calls = 0;
                _frames = 0;
                _maxBlock = 0;
                _outSumSq = 0.0;
                _outSamples = 0;
            }
        }

        // オーディオスレッド (DSP): リングからレート変換した音声を最終ミックスへ加算する。
        // data は出力スピーカーの interleaved (長さ = フレーム数 * channels)。
        private void OnAudioFilterRead(float[] data, int channels)
        {
            var ring = _ring;
            var scratch = _scratch;
            if (ring == null || _srcChannels <= 0 || scratch == null || channels <= 0)
                return; // 何もしない = 無音 (キャリアの 0) のまま

            int frames = data.Length / channels;
            int need = frames * _srcChannels;
            if (need > scratch.Length) return; // 想定外の巨大ブロックは安全側でスキップ

            // src チャネルで補間しつつ取り出す。
            ring.Read(scratch, frames, _baseStep);

            // 出力検証用の RMS は per-sample コストになるため診断ログ有効時のみ集計する。
            bool log = CefLog.Enabled;
            double sumSq = 0.0;
            if (log)
                for (int i = 0; i < need; i++)
                {
                    float s = scratch[i];
                    sumSq += (double)s * s;
                }

            // 最終ミックスへ加算。src と出力のチャネル数が同じなら直接、違えば写像する。
            if (channels == _srcChannels)
            {
                int n = frames * channels;
                for (int i = 0; i < n; i++) data[i] += scratch[i];
            }
            else
            {
                for (int f = 0; f < frames; f++)
                for (int c = 0; c < channels; c++)
                    data[f * channels + c] += scratch[f * _srcChannels + c % _srcChannels];
            }

            if (log)
                lock (_statsLock)
                {
                    _calls++;
                    _frames += frames;
                    if (frames > _maxBlock) _maxBlock = frames;
                    _outSumSq += sumSq;
                    _outSamples += need;
                }
        }
    }
}
