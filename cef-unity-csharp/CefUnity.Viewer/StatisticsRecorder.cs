using System;
using System.IO;

namespace CefUnity.Viewer
{
    /// <summary>フレーム毎の計測 CSV (spec §StatisticsRecorder)。paint fps は paint_frame_id の差分から後段解析。</summary>
    internal sealed class StatisticsRecorder : IDisposable
    {
        public const string Header = "frame,dt_milliseconds,paint_frame_id,sent_delta_x,sent_delta_y,mode";

        private readonly StreamWriter _writer;

        public StatisticsRecorder(string path)
        {
            _writer = new StreamWriter(path);
            _writer.WriteLine(Header);
        }

        public void RecordFrame(long frameIndex, double deltaTimeMilliseconds, ulong paintFrameId,
            int sentDeltaX, int sentDeltaY, ScrollMode mode)
        {
            _writer.WriteLine(FormattableString.Invariant(
                $"{frameIndex},{deltaTimeMilliseconds:F3},{paintFrameId},{sentDeltaX},{sentDeltaY},{mode}"));
        }

        public void Dispose() => _writer.Dispose();
    }
}
