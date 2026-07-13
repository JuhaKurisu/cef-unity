using System;
using CefUnity.Interop;
using UnityEngine;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     CRI 方式のネイティブ音声出力を管理するコンポーネント (macOS)。
    ///     <para>
    ///     Unity の FMOD ミキサを迂回し、client dylib 内の AudioUnit で直接再生する
    ///     低遅延経路 (内部 ~30ms 級。<see cref="CefAudioOutput" /> の Unity ミキサ経路は
    ///     ~160ms)。任意の GameObject にアタッチして <see cref="Browser" /> を設定すると、
    ///     ストリーム開始を検出して自動的にネイティブ再生を開始する。
    ///     </para>
    ///     <para>
    ///     注意:
    ///     - <see cref="CefAudioOutput" /> と併用すると二重再生になる (どちらか一方のみ)。
    ///     - AudioMixer エフェクト・スペーシャライズ・オーディオプロファイラ表示は効かない。
    ///     - AudioListener.volume / pause は Update で音量に反映する。
    ///     - PCM の取得 (録画等) は <see cref="Interop.Browser.ReadAudio" /> がカーソル独立で併用可能。
    ///     </para>
    /// </summary>
    public class CefNativeAudio : MonoBehaviour
    {
        [SerializeField] [Range(0f, 1f)] private float _volume = 1f;

        [Tooltip("jitter buffer の目標滞留量 (ms)。下げるほど低遅延だがスパイクに弱い")]
        [SerializeField] private float _targetLatencyMs = 15f;

        [Tooltip("CoreAudio IO バッファフレーム数 (128 ≈ 2.9ms)。デバイス共有設定なので他アプリにも影響する")]
        [SerializeField] private int _ioFrames = 128;

        /// <summary>再生対象のブラウザ。外部から設定する。</summary>
        public Browser Browser { get; set; }

        /// <summary>ネイティブ再生中か。</summary>
        public bool IsPlaying { get; private set; }

        private int _srcChannels;
        private float _lastSentVolume = float.NaN;
        private float _diagTimer;
        private ulong _lastUnderrun;
        private ulong _lastOverflow;

        private void Update()
        {
            if (Browser == null) return;

            if (!IsPlaying)
            {
                TryStart();
                return;
            }

            // チャネル数が変わったら再起動 (native 側は無音を出して待っている)。
            // Browser dispose 直後の 1 フレームと競合し得るので防御的に握りつぶす
            // (サンプルは Browser=null → dispose の順序を保証するが、単体利用に備える)。
            try
            {
                if (Browser.TryGetAudioFormat(out _, out int ch) && ch > 0 && ch != _srcChannels)
                {
                    Stop();
                    TryStart();
                    return;
                }

                SyncVolume();
                LogDiagnostics();
            }
            catch (ObjectDisposedException)
            {
                IsPlaying = false;
            }
        }

        /// <summary>ストリームフォーマットが確定していればネイティブ再生を開始する。</summary>
        private void TryStart()
        {
            bool active;
            int sampleRate, channels;
            try
            {
                active = Browser.TryGetAudioFormat(out sampleRate, out channels);
            }
            catch (Exception)
            {
                return;
            }

            if (!active || sampleRate <= 0 || channels <= 0) return;
            if (!Browser.StartNativeAudio(_targetLatencyMs, _ioFrames)) return;

            _srcChannels = channels;
            IsPlaying = true;
            _lastSentVolume = float.NaN; // 次の Update で必ず音量を送る
            if (CefLog.Enabled)
                CefLog.Log($"[CefAudio-NAT] start {sampleRate}Hz x{channels}ch " +
                           $"target={_targetLatencyMs}ms ioFrames={_ioFrames}");
        }

        // AudioListener の音量/ポーズと Inspector 音量を native 側へ同期する。
        private void SyncVolume()
        {
            float v = AudioListener.pause ? 0f : AudioListener.volume * _volume;
            if (!float.IsNaN(_lastSentVolume) && Mathf.Approximately(v, _lastSentVolume)) return;
            Browser.SetNativeAudioVolume(v);
            _lastSentVolume = v;
        }

        // 1 秒ごとに滞留量とアンダーラン/オーバーフローをログ出力する診断。
        private void LogDiagnostics()
        {
            if (!CefLog.Enabled) return;
            _diagTimer += Time.unscaledDeltaTime;
            if (_diagTimer < 1f) return;
            _diagTimer = 0f;

            if (!Browser.TryGetNativeAudioStats(out float occMs, out ulong under, out ulong over))
                return;
            ulong underDelta = under - _lastUnderrun;
            ulong overDelta = over - _lastOverflow;
            _lastUnderrun = under;
            _lastOverflow = over;
            CefLog.Log(
                $"[CefAudio-NAT] occ={occMs:F1}ms (target={_targetLatencyMs:F1}ms) | " +
                $"underrun/s={underDelta} overflow/s={overDelta} (total under={under} over={over})");
        }

        private void Stop()
        {
            if (!IsPlaying) return;
            IsPlaying = false;
            try
            {
                Browser?.StopNativeAudio();
            }
            catch (Exception)
            {
                // Browser が先に dispose されていても Rust 側 destroy が voice を停止済み。
            }
        }

        private void OnDisable()
        {
            Stop();
        }
    }
}
