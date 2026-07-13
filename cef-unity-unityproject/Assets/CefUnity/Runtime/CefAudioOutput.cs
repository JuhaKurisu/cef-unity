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
    ///        ポーリングし、PCM を <see cref="CefAudioRing" /> へ蓄積する (producer)。
    ///     2. 専用の子 GameObject 上の <see cref="CefAudioSink" /> が <c>OnAudioFilterRead</c>
    ///        (オーディオスレッド) で <see cref="CefAudioRing.Read" /> を呼び、CEF レート →
    ///        Unity 出力レートの変換と滞留量補正を行いながら排出して出力ミックスへ加算する (consumer)。
    ///     </para>
    ///     <para>
    ///     CEF と Unity はオーディオクロックが独立しているためレートがわずかにずれる。
    ///     固定レートで読むと滞留量がドリフトしてアンダーラン (無音) かオーバーフロー
    ///     (破棄) でぶつ切りになる。これを防ぐためレート変換と滞留量制御は
    ///     <see cref="CefAudioRing" /> に集約してある (詳細はそちらを参照)。
    ///     </para>
    ///     <para>
    ///     消費に <see cref="AudioClip" /> のストリーミング (PCMReaderCallback) を使うと
    ///     先読みバッファ管理の都合で消費レートが波打ち (実測 800〜1200ms/s)、滞留量が振動して
    ///     アンダーランの原因になる。そのため一定ペースで呼ばれる <c>OnAudioFilterRead</c> を
    ///     <see cref="CefAudioSink" /> 経由で使う。
    ///     </para>
    ///     <para>
    ///     PCM を自前で扱いたい (録音・解析・独自ミキサ等) 場合は、本コンポーネントを
    ///     使わず <see cref="Browser.ReadAudio" /> を直接呼べばストリームをそのまま取得できる。
    ///     </para>
    /// </summary>
    public class CefAudioOutput : MonoBehaviour
    {
        /// <summary>1 回の Update でブラウザから取り込む最大フレーム数。</summary>
        private const int MaxPullFrames = 8192;

        /// <summary>リングバッファの容量 (秒)。オーバーフローのバックストップ。</summary>
        [SerializeField] private float _bufferSeconds = 0.5f;

        /// <summary>
        ///     狙う滞留量 (秒)。これがそのまま ④ポーリング+⑤リング の再生遅延になる。
        ///     メインスレッドのフレームスパイク (GC 等による Update 停止) を吸収できる
        ///     だけの長さが必要。下回るとアンダーラン (無音) でぶつ切りになる。
        /// </summary>
        [SerializeField] private float _targetLatencySeconds = 0.08f;

        /// <summary>
        ///     消費レートの操作上限 (0.01 = ±1%)。滞留量を目標へ収束させるための
        ///     非同期サンプルレート変換の steering 量。大きいほど速く補正できるが
        ///     ピッチ揺れが目立つ。1% で約 17 cents (最大誤差時のみ・通常は無視できる)。
        /// </summary>
        [SerializeField] private float _maxRateAdjust = 0.02f;

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

        /// <summary>累積アンダーラン (無音埋め) フレーム数。0 以外ならぶつ切り発生。</summary>
        public long UnderrunFrames => _ring?.UnderrunFrames ?? 0;

        /// <summary>累積オーバーフロー (破棄) フレーム数。0 以外なら容量超過。</summary>
        public long OverflowDropFrames => _ring?.OverflowDropFrames ?? 0;

        // ReadAudio 用のスクラッチ (interleaved, 最大 AUDIO_MAX_CHANNELS=8 ストライド)。
        private float[] _pullScratch;

        // CEF→Unity レート変換つきリング (メインスレッドが書き、オーディオスレッドが読む)。
        private CefAudioRing _ring;

        // 出力ミックスへ流す consumer。AudioListener と OnAudioFilterRead が競合しないよう
        // 専用の子 GameObject 上に分離する。
        private CefAudioSink _sink;

        private int _srcSampleRate;
        private int _srcChannels;
        private double _baseStep; // srcRate / outRate (出力1フレームあたり進める src フレーム数)
        private bool _streamReady;

        private void Awake()
        {
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

            LogDiagnostics();
        }

        // 1 秒ごとに音声経路の状態をログ出力する診断 (マスターログフラグ CefLog.Enabled に従う)。
        private float _diagTimer;
        private float[] _spectrum;
        private long _lastUnderrun;
        private long _lastOverflow;

        // producer (PullFromBrowser) のバースト性を測る計装 (1秒ごとにリセット)。
        private int _pullCalls;     // Update 中に ReadAudio を呼んだ回数
        private int _pullZero;      // got==0 だった回数
        private int _pullFramesSum; // 取得フレーム合計
        private int _pullMax;       // 1回の最大取得フレーム
        private int _pullMin = int.MaxValue; // 1回の最小 (>0) 取得フレーム

        private void LogDiagnostics()
        {
            if (!CefLog.Enabled) return;
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

            // ----- 遅延・健全性の実測 -----
            double occFrames = _ring?.OccupancyFrames ?? 0;
            float ringMs = _srcSampleRate > 0 ? (float)(occFrames / _srcSampleRate * 1000.0) : 0f;
            float targetMs = (_ring != null && _srcSampleRate > 0)
                ? (float)_ring.TargetFrames / _srcSampleRate * 1000f
                : 0f;

            // 直近 1 秒のアンダーラン / オーバーフロー (= ぶつ切りの直接指標)。
            long under = _ring?.UnderrunFrames ?? 0;
            long over = _ring?.OverflowDropFrames ?? 0;
            long underDelta = under - _lastUnderrun;
            long overDelta = over - _lastOverflow;
            _lastUnderrun = under;
            _lastOverflow = over;

            // Unity DSP ミキサのバッファリング (= ⑧)。dspLen サンプル × numBuffers 段。
            AudioSettings.GetDSPBufferSize(out int dspLen, out int dspNum);
            float dspMs = outRate > 0 ? (float)dspLen * dspNum / outRate * 1000f : 0f;

            // メインスレッドのフレーム間隔 (= ④ポーリング周期の実測)。
            float frameMs = Time.smoothDeltaTime * 1000f;

            // CEF キャプチャバッファ (= ①)。frames_per_buffer=512 固定 (server.rs と要同期)。
            float cefCaptureMs = _srcSampleRate > 0 ? 512f / _srcSampleRate * 1000f : 0f;

            CefLog.Log(
                $"[CefAudio] {_srcSampleRate}Hz x{_srcChannels}ch recvFrames={TotalFramesReceived} " +
                $"rms={LastPulledRms:F3} peak={LastPulledPeak:F3} spec={peakFreq:F0}Hz(m={peakMag:F3})");
            CefLog.Log(
                $"[CefAudio-LAT] ringOcc={ringMs:F1}ms (target={targetMs:F1}ms) | " +
                $"underrun/s={underDelta} overflow/s={overDelta} (total under={under} over={over}) | " +
                $"frameInterval={frameMs:F1}ms | " +
                $"dspBuf={dspLen}x{dspNum}={dspMs:F1}ms@{outRate}Hz | " +
                $"cefCapture={cefCaptureMs:F1}ms | " +
                $"sum(①④⑤⑧)≈{cefCaptureMs + frameMs + ringMs + dspMs:F1}ms (excl. Unity stream readahead + HW)");

            // producer (ReadAudio) のバースト性。理想は毎フレーム ~800 frames (16.7ms@48k) で
            // min≒max。min が 0 近く / max が大きく振れるならバースト供給 = 滞留量振動の原因。
            int nonZero = _pullCalls - _pullZero;
            float avgPull = nonZero > 0 ? (float)_pullFramesSum / nonZero : 0f;
            int pullMin = _pullMin == int.MaxValue ? 0 : _pullMin;
            float pullMaxMs = _srcSampleRate > 0 ? _pullMax / (float)_srcSampleRate * 1000f : 0f;
            CefLog.Log(
                $"[CefAudio-PROD] pulls/s={_pullCalls} zero={_pullZero} | framesGot avg={avgPull:F0} " +
                $"min={pullMin} max={_pullMax} (max={pullMaxMs:F1}ms) | sum={_pullFramesSum} (~{(_srcSampleRate > 0 ? _pullFramesSum / (float)_srcSampleRate * 1000f : 0f):F0}ms/s)");
            _pullCalls = 0;
            _pullZero = 0;
            _pullFramesSum = 0;
            _pullMax = 0;
            _pullMin = int.MaxValue;

            // consumer (CefAudioSink.OnAudioFilterRead) の安定性。calls/s が一定・maxBlock が
            // 固定なら一定ペースで消費できている。outRms>0 ならミックスへ音声が出ている。
            if (_sink != null)
            {
                _sink.SnapshotStats(out int pcmCalls, out int pcmFrames, out int pcmMax, out double outSumSq, out long outSamples);
                float pcmAvg = pcmCalls > 0 ? (float)pcmFrames / pcmCalls : 0f;
                float outRms = outSamples > 0 ? (float)Math.Sqrt(outSumSq / outSamples) : 0f;
                CefLog.Log(
                    $"[CefAudio-CONS] calls/s={pcmCalls} framesOut/s={pcmFrames} (~{(outRate > 0 ? pcmFrames / (float)outRate * 1000f : 0f):F0}ms/s) " +
                    $"avg={pcmAvg:F0} maxBlock={pcmMax} | outRms={outRms:F3} (ミックスへ加算した音声; 0なら出力経路断)");
            }
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

            int outRate = AudioSettings.outputSampleRate;
            if (outRate <= 0) outRate = sampleRate;

            _srcSampleRate = sampleRate;
            _srcChannels = channels;
            _baseStep = (double)sampleRate / outRate;

            // レート変換つきリングを確保。容量 = _bufferSeconds、目標 = _targetLatencySeconds。
            int capFrames = Mathf.Max(2, Mathf.CeilToInt(_bufferSeconds * sampleRate));
            int targetFrames = Mathf.Clamp(Mathf.CeilToInt(_targetLatencySeconds * sampleRate), 1, capFrames - 1);
            _ring = new CefAudioRing(capFrames, channels, targetFrames, _maxRateAdjust);

            // 消費は専用子 GameObject 上の CefAudioSink (OnAudioFilterRead) が行う。
            // AudioListener を持つ本 GameObject 上に OnAudioFilterRead を置くとバインド先が
            // 非決定的になり動作が不安定になるため、AudioSource とだけ同居する子に分離する。
            if (_sink == null)
            {
                var go = new GameObject("CefAudioSink");
                go.transform.SetParent(transform, false);
                _sink = go.AddComponent<CefAudioSink>();
            }

            _sink.Configure(_ring, _baseStep, channels, outRate, MaxPullFrames);
            _streamReady = true;
            return true;
        }

        /// <summary>
        ///     メインスレッド: ブラウザの未読 PCM をリングへ取り込む (producer)。
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

            // producer バースト性の計装。
            _pullCalls++;
            if (got <= 0) _pullZero++;
            else
            {
                _pullFramesSum += got;
                if (got > _pullMax) _pullMax = got;
                if (got < _pullMin) _pullMin = got;
            }

            if (got <= 0) return;

            // ストリームのチャネル数が変わったら作り直す (稀)。
            if (ch != _srcChannels && ch > 0)
            {
                _streamReady = false;
                _ring = null;
                if (_sink != null) _sink.StopOutput();
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

            _ring?.Write(_pullScratch, 0, got);
        }

        private void OnDisable()
        {
            if (_sink != null) _sink.StopOutput();
        }
    }
}
