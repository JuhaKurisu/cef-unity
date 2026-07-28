using System;
using CefUnity.Runtime;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     スクロール 3 モードの切替と毎フレーム排出 (spec §ScrollInputMatrix)。
    ///     ①Raw: 窓 wheel を 1:1 直送 (平滑化前の旧 Unity 相当)
    ///     ②Smoother: 窓 wheel → ScrollSmoother (Unity A案)
    ///     ③Resampler: native/replay ソース → ScrollResampler 予測 (Unity C案 = 現行既定)
    ///     native ソースの Drain は全モードで行う (録画のため) が、転送は Resampler モードのみ。
    /// </summary>
    public sealed class ScrollInputMatrix : IDisposable
    {
        private readonly ScrollInputPipeline _pipeline = new ScrollInputPipeline();
        private float _rawPendingX;
        private float _rawPendingY;

        public ScrollMode Mode { get; private set; } = ScrollMode.Resampler;

        public NativeScrollSourceStart StartNativeSource(out Exception? error) => _pipeline.StartNativeSource(out error!);

        public void AttachSource(IScrollEventSource source) => _pipeline.AttachSource(source);

        public bool RecordingEnabled
        {
            get => _pipeline.RecordingEnabled;
            set => _pipeline.RecordingEnabled = value;
        }

        public void SetMode(ScrollMode mode)
        {
            Mode = mode;
            _pipeline.Reset();
            _rawPendingX = 0f;
            _rawPendingY = 0f;
        }

        public void AddWheelSteps(float xSteps, float ySteps)
        {
            switch (Mode)
            {
                case ScrollMode.Raw:
                    _rawPendingX += xSteps * ScrollInputPipeline.WheelPixelsPerStep;
                    _rawPendingY += ySteps * ScrollInputPipeline.WheelPixelsPerStep;
                    break;
                case ScrollMode.Smoother:
                    _pipeline.AddWheelSteps(xSteps, ySteps, resolutionScale: 1f);
                    break;
                // Resampler: 窓 wheel は無視 (native ソースが同じ物理イベントを拾う — 二重計上防止)
            }
        }

        public void TickFrame(float deltaTimeSeconds, bool overBrowser,
            out int primaryDeltaX, out int primaryDeltaY, out int secondaryDeltaX, out int secondaryDeltaY)
        {
            primaryDeltaX = 0;
            primaryDeltaY = 0;
            secondaryDeltaX = 0;
            secondaryDeltaY = 0;
            // 順序は Pipeline 規約: Drain → TickResampler → (送信) → TickSmoother → (送信)
            _pipeline.Drain(overBrowser && Mode == ScrollMode.Resampler, resolutionScale: 1f);
            switch (Mode)
            {
                case ScrollMode.Resampler:
                    _pipeline.TickResampler(out primaryDeltaX, out primaryDeltaY);
                    // 非 precise (ホイールノッチ) は Drain がスムーザへ回すので secondary で排出
                    _pipeline.TickSmoother(deltaTimeSeconds, out secondaryDeltaX, out secondaryDeltaY);
                    break;
                case ScrollMode.Smoother:
                    _pipeline.TickSmoother(deltaTimeSeconds, out primaryDeltaX, out primaryDeltaY);
                    break;
                case ScrollMode.Raw:
                    primaryDeltaX = ConsumeWhole(ref _rawPendingX);
                    primaryDeltaY = ConsumeWhole(ref _rawPendingY);
                    break;
            }
        }

        private static int ConsumeWhole(ref float pending)
        {
            var whole = (int)pending;
            pending -= whole;
            return whole;
        }

        public void Dispose() => _pipeline.Dispose();
    }
}
