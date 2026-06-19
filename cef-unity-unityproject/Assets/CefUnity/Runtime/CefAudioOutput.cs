using System;
using CefUnity.Interop;
using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     CEF から送られてきた音声 PCM を Unity の AudioSource で再生するコンポーネント。
    ///     <para>
    ///     使い方: 任意の GameObject にアタッチし、<see cref="Browser" /> プロパティに
    ///     再生したいブラウザを設定するだけ。ストリームが始まると自動的にスピーカーから
    ///     音が出る (CEF/ブラウザプロセス側では鳴らず、Unity 側のみで再生される)。
    ///     </para>
    ///     <para>
    ///     仕組み:
    ///     1. メインスレッド (<see cref="Update" />) で <see cref="Browser.ReadAudio" /> を
    ///        ポーリングし、PCM を内部リングバッファへ蓄積する。
    ///     2. ストリーミング <see cref="AudioClip" /> (CEF のサンプリングレートで生成) の
    ///        PCM read コールバック (オーディオスレッド) がリングバッファを排出する。
    ///        CEF のレート → Unity 出力レートの変換は Unity が自動で行う。
    ///     </para>
    ///     <para>
    ///     PCM を自前で扱いたい (録音・解析・独自ミキサ等) 場合は、本コンポーネントを
    ///     使わず <see cref="Browser.ReadAudio" /> を直接呼べばストリームをそのまま取得できる。
    ///     </para>
    /// </summary>
    [RequireComponent(typeof(AudioSource))]
    public class CefAudioOutput : MonoBehaviour
    {
        /// <summary>1 回の Update でブラウザから取り込む最大フレーム数。</summary>
        private const int MaxPullFrames = 8192;

        /// <summary>リングバッファの長さ (秒)。フレームレートのジッタを吸収する。</summary>
        [SerializeField] private float _bufferSeconds = 0.5f;

        /// <summary>
        ///     診断ログを有効にする。1 秒ごとに受信量・RMS・出力スペクトルの
        ///     ピーク周波数を Debug.Log へ出力する (音声経路の定量確認用)。
        /// </summary>
        [SerializeField] private bool _logDiagnostics = true;

        /// <summary>再生対象のブラウザ。外部から設定する。</summary>
        public Browser Browser { get; set; }

        // ----- 診断 (音声経路が機能しているかの定量確認用) -----

        /// <summary>これまでにブラウザから取り込んだ累積フレーム数。0 のままなら CEF→Unity 経路が未機能。</summary>
        public long TotalFramesReceived { get; private set; }

        /// <summary>直近に取り込んだ PCM の RMS レベル (0=無音)。</summary>
        public float LastPulledRms { get; private set; }

        /// <summary>直近に取り込んだ PCM のピーク絶対値 (0=無音)。</summary>
        public float LastPulledPeak { get; private set; }

        /// <summary>現在のストリームのサンプリングレート (Hz)。未確定は 0。</summary>
        public int SourceSampleRate => _srcSampleRate;

        /// <summary>現在のストリームのチャネル数。未確定は 0。</summary>
        public int SourceChannels => _srcChannels;

        private AudioSource _audioSource;

        // ReadAudio 用のスクラッチ (interleaved, 最大 AUDIO_MAX_CHANNELS=8 ストライド)。
        private float[] _pullScratch;

        // メインスレッドが書き、オーディオスレッドが読むリングバッファ (interleaved)。
        private float[] _ring;
        private int _ringCapacity; // サンプル数 (= フレーム数 * channels)
        private long _ringWrite;   // 累積書き込みサンプル数
        private long _ringRead;    // 累積読み出しサンプル数
        private readonly object _ringLock = new object();

        private int _srcSampleRate;
        private int _srcChannels;
        private bool _streamReady;

        private void Awake()
        {
            _audioSource = GetComponent<AudioSource>();
            _audioSource.playOnAwake = false;
            _audioSource.loop = true;
            _audioSource.spatialBlend = 0f; // 2D (UI ブラウザ音声を想定)
            _pullScratch = new float[MaxPullFrames * 8];
        }

        private void Update()
        {
            if (Browser == null) return;

            // フォーマット確定前: ストリーム開始を待つ。
            if (!_streamReady)
            {
                if (!TryInitStream()) return;
            }

            PullFromBrowser();

            if (_logDiagnostics) LogDiagnostics();
        }

        // 1 秒ごとに音声経路の状態をログ出力する診断。
        private float _diagTimer;
        private float[] _spectrum;

        private void LogDiagnostics()
        {
            _diagTimer += Time.unscaledDeltaTime;
            if (_diagTimer < 1f) return;
            _diagTimer = 0f;

            // 出力スペクトルのピーク周波数を求める (Unity 出力 → スピーカー経路の確認)。
            _spectrum ??= new float[1024];
            float peakFreq = 0f, peakMag = 0f;
            AudioListener.GetSpectrumData(_spectrum, 0, FFTWindow.BlackmanHarris);
            int n = _spectrum.Length;
            int outRate = AudioSettings.outputSampleRate;
            for (int i = 1; i < n; i++)
            {
                if (_spectrum[i] > peakMag)
                {
                    peakMag = _spectrum[i];
                    // bin i の中心周波数 = i * (outRate/2) / n
                    peakFreq = i * (outRate * 0.5f) / n;
                }
            }

            Debug.Log(
                $"[CefAudio] active stream {_srcSampleRate}Hz x{_srcChannels}ch | " +
                $"recvFrames={TotalFramesReceived} pulledRms={LastPulledRms:F4} pulledPeak={LastPulledPeak:F4} | " +
                $"outSpectrumPeak={peakFreq:F0}Hz(mag={peakMag:F4}) outRate={outRate}");
        }

        /// <summary>
        ///     ストリームフォーマットが確定したら AudioClip を生成して再生開始する。
        ///     成功したら true。
        /// </summary>
        private bool TryInitStream()
        {
            bool active;
            int sampleRate, channels;
            try
            {
                active = Browser.TryGetAudioFormat(out sampleRate, out channels);
            }
            catch (Exception)
            {
                return false;
            }

            if (!active || sampleRate <= 0 || channels <= 0) return false;

            _srcSampleRate = sampleRate;
            _srcChannels = channels;

            // リングバッファ確保 (秒数 × レート × チャネル)。
            _ringCapacity = Mathf.Max(1, Mathf.CeilToInt(_bufferSeconds * sampleRate)) * channels;
            _ring = new float[_ringCapacity];
            _ringWrite = 0;
            _ringRead = 0;

            // CEF のサンプリングレート・チャネル数でストリーミングクリップを生成。
            // Unity が出力デバイスのレートへ自動リサンプルする。
            // lengthSamples はクリップの「1 周分」の長さ (ループ用)。1 秒分を確保。
            var clip = AudioClip.Create(
                "CefAudio",
                sampleRate,        // lengthSamples (per channel)
                channels,
                sampleRate,
                true,
                OnPcmRead);

            _audioSource.clip = clip;
            _audioSource.Play();
            _streamReady = true;
            return true;
        }

        /// <summary>
        ///     メインスレッド: ブラウザの未読 PCM をリングバッファへ取り込む。
        /// </summary>
        private void PullFromBrowser()
        {
            int got;
            int ch;
            try
            {
                got = Browser.ReadAudio(_pullScratch, MaxPullFrames, out ch);
            }
            catch (Exception)
            {
                return;
            }

            if (got <= 0) return;

            // ストリームのチャネル数が変わったら作り直す (稀)。
            if (ch != _srcChannels && ch > 0)
            {
                _streamReady = false;
                return;
            }

            int samples = got * _srcChannels;

            // 診断: 受信量と直近 RMS/ピークを更新。
            TotalFramesReceived += got;
            double sumSq = 0.0;
            float peak = 0f;
            for (int i = 0; i < samples; i++)
            {
                float s = _pullScratch[i];
                sumSq += (double)s * s;
                float a = s < 0f ? -s : s;
                if (a > peak) peak = a;
            }
            LastPulledRms = samples > 0 ? (float)Math.Sqrt(sumSq / samples) : 0f;
            LastPulledPeak = peak;
            lock (_ringLock)
            {
                long available = _ringWrite - _ringRead;
                long freeSpace = _ringCapacity - available;
                if (samples > _ringCapacity)
                {
                    // パケットが容量を超える: 末尾だけ残す。
                    int srcOffset = samples - _ringCapacity;
                    WriteRing(_pullScratch, srcOffset, _ringCapacity);
                    // 読みカーソルを最新位置へ。
                    _ringRead = _ringWrite - _ringCapacity;
                }
                else
                {
                    if (samples > freeSpace)
                    {
                        // オーバーフロー: 最古を捨てる。
                        _ringRead += samples - freeSpace;
                    }
                    WriteRing(_pullScratch, 0, samples);
                }
            }
        }

        // _ringLock 保持下で呼ぶこと。src[srcOffset..srcOffset+count] をリングへ書く。
        private void WriteRing(float[] src, int srcOffset, int count)
        {
            for (int i = 0; i < count; i++)
            {
                _ring[(int)(_ringWrite % _ringCapacity)] = src[srcOffset + i];
                _ringWrite++;
            }
        }

        /// <summary>
        ///     オーディオスレッド: リングバッファから data を満たす。不足分は無音。
        ///     data は interleaved で長さ = フレーム数 * _srcChannels。
        /// </summary>
        private void OnPcmRead(float[] data)
        {
            int filled = 0;
            lock (_ringLock)
            {
                long available = _ringWrite - _ringRead;
                int n = (int)Math.Min(data.Length, available);
                for (int i = 0; i < n; i++)
                {
                    data[i] = _ring[(int)(_ringRead % _ringCapacity)];
                    _ringRead++;
                }
                filled = n;
            }
            // アンダーラン: 残りを無音で埋める。
            for (int i = filled; i < data.Length; i++) data[i] = 0f;
        }

        private void OnDisable()
        {
            if (_audioSource != null) _audioSource.Stop();
        }
    }
}
