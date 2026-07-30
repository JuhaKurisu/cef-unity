using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using CefUnity.Interop;

namespace CefUnity.Harness
{

/// <summary>
///     Unity を使わずに GPU 経路 (macOS: IOSurface / Metal) の paint 供給レートを測る診断コマンド。
///
///     <para>
///     目的は issue #7 の再現である。<c>on_accelerated_paint</c> 内の同期 GPU コピー完了待ちは
///     サーバーのメッセージ pump スレッド上で実行されるため、外部 GPU 競合でコピーが伸びると
///     pump ごと止まり、paint 供給が崩壊する。ここでは Unity の代わりに
///     60Hz で SendExternalBeginFrame + Pump を回し、client 側に届くフレーム数と
///     フレーム間の最大ギャップを秒毎に出す。サーバー側の STATISTICS 行
///     (pump tick 数 / copy_wait) と突き合わせることで因果を確認できる。
///     </para>
/// </summary>
internal static class PaintStatisticsCommand
{
    /// <summary>rAF で毎フレーム全画面の色を変えるページ。全面 damage を作り paint を毎フレーム発生させる。</summary>
    private const string AnimationHtml = """
        <!doctype html><meta charset="utf-8">
        <body style="margin:0;overflow:hidden">
        <div id="surface" style="width:100vw;height:100vh"></div>
        <script>
        let step = 0;
        const surface = document.getElementById('surface');
        function frame() {
            step = (step + 1) % 256;
            surface.style.background =
                'rgb(' + step + ',' + ((step * 7) % 256) + ',' + ((step * 13) % 256) + ')';
            requestAnimationFrame(frame);
        }
        requestAnimationFrame(frame);
        </script>
        </body>
        """;

    public static int Run(int durationSeconds, int viewportWidth = 1280, int viewportHeight = 720)
    {
        var pagePath = Path.Combine(Path.GetTempPath(), "cef_unity_paint_statistics.html");
        File.WriteAllText(pagePath, AnimationHtml);
        var url = new Uri(pagePath).AbsoluteUri;

        CefRuntime.Initialize(useGpu: true, enableLog: true);
        try
        {
            using var browser = new Browser(viewportWidth, viewportHeight, url);
            Console.WriteLine($"viewport={viewportWidth}x{viewportHeight}");

            // ページのロードと GPU 経路の接続を待つ (2 秒相当)。
            var beginFrameIndex = 0UL;
            for (var warmupIndex = 0; warmupIndex < 120; warmupIndex++)
            {
                browser.SendExternalBeginFrame(beginFrameIndex++);
                CefRuntime.Pump();
                DrainReceivedTexture();
                Thread.Sleep(16);
            }
            Console.WriteLine($"iosurface_connected={Browser.IsIOSurfaceConnected()}");

            var frameInterval = TimeSpan.FromTicks(TimeSpan.TicksPerSecond / 60);
            var clock = Stopwatch.StartNew();
            var deadline = clock.Elapsed;
            var windowStart = clock.Elapsed;
            var lastReceivedAt = clock.Elapsed;
            var receivedInWindow = 0;
            var beginFramesInWindow = 0;
            var maximumGapMilliseconds = 0.0;
            var perSecondReceived = new List<int>();
            var perSecondMaximumGap = new List<double>();

            while (clock.Elapsed < TimeSpan.FromSeconds(durationSeconds))
            {
                deadline += frameInterval;

                browser.SendExternalBeginFrame(beginFrameIndex++);
                beginFramesInWindow++;
                CefRuntime.Pump();

                if (DrainReceivedTexture())
                {
                    var gap = (clock.Elapsed - lastReceivedAt).TotalMilliseconds;
                    if (gap > maximumGapMilliseconds) maximumGapMilliseconds = gap;
                    lastReceivedAt = clock.Elapsed;
                    receivedInWindow++;
                }

                if (clock.Elapsed - windowStart >= TimeSpan.FromSeconds(1))
                {
                    // 受信が完全に途切れている間もギャップは伸びるので、窓を閉じる時点でも測る。
                    var pendingGap = (clock.Elapsed - lastReceivedAt).TotalMilliseconds;
                    if (pendingGap > maximumGapMilliseconds) maximumGapMilliseconds = pendingGap;

                    Console.WriteLine(
                        $"t={clock.Elapsed.TotalSeconds,5:F1}s begin_frames={beginFramesInWindow,3} " +
                        $"received={receivedInWindow,3} max_gap={maximumGapMilliseconds,7:F1}ms");
                    perSecondReceived.Add(receivedInWindow);
                    perSecondMaximumGap.Add(maximumGapMilliseconds);
                    receivedInWindow = 0;
                    beginFramesInWindow = 0;
                    maximumGapMilliseconds = 0;
                    windowStart = clock.Elapsed;
                }

                var remaining = deadline - clock.Elapsed;
                if (remaining > TimeSpan.Zero) Thread.Sleep(remaining);
                else deadline = clock.Elapsed; // 遅れた分は取り戻さない (burst 注入を避ける)
            }

            Console.WriteLine();
            Console.WriteLine(
                $"CLIENT_SUMMARY seconds={perSecondReceived.Count} " +
                $"received_min={(perSecondReceived.Count > 0 ? perSecondReceived.Min() : 0)} " +
                $"received_median={Median(perSecondReceived)} " +
                $"received_max={(perSecondReceived.Count > 0 ? perSecondReceived.Max() : 0)} " +
                $"gap_max={(perSecondMaximumGap.Count > 0 ? perSecondMaximumGap.Max() : 0):F1}ms " +
                $"verified_frames={s_verifiedFrameCount} torn_frames={s_tornFrameCount} " +
                $"distinct_colors={s_observedColors.Count} max_colors_in_frame={s_maximumColorsInFrame}");

            Console.WriteLine();
            foreach (var line in CefRuntime.GetLogs().Where(line => line.Contains("STATISTICS")))
                Console.WriteLine($"SERVER {line}");

            return perSecondReceived.Count > 0 ? 0 : 1;
        }
        finally
        {
            CefRuntime.Shutdown();
        }
    }

    /// <summary>1 フレームあたりのティアリング検査でサンプルする行数。</summary>
    private const int SampleRowCount = 16;
    private static readonly uint[] s_samplePixels = new uint[SampleRowCount];

    /// <summary>ティアリング (1 フレーム内に複数の色が混在) を検出したフレーム数。</summary>
    private static int s_tornFrameCount;
    /// <summary>サンプル取得に成功し検査できたフレーム数。</summary>
    private static int s_verifiedFrameCount;
    /// <summary>観測した先頭画素の異なる値の数 (検出器が生きているかの確認用)。</summary>
    private static readonly HashSet<uint> s_observedColors = new();
    /// <summary>1 フレーム内で観測した最大の色数 (2 以上ならティアリング)。</summary>
    private static int s_maximumColorsInFrame = 1;

    /// <summary>Mach 経由で届いた最新 IOSurface を受け取り、即解放する。届いていれば true。</summary>
    private static bool DrainReceivedTexture()
    {
        if (!Browser.TryReceiveIOSurfaceTexture(out var texture, out _, out _, out _)) return false;

        // 全画面単色ページなので 1 フレーム内の全サンプルは同じ値になるはず。
        // 割れていれば blit 未完了の転送先を読んだ / src が上書きされた証拠 (ティアリング)。
        if (Browser.SampleIOSurfacePixels(s_samplePixels) == SampleRowCount)
        {
            s_verifiedFrameCount++;
            s_observedColors.Add(s_samplePixels[0]);
            var colorsInFrame = s_samplePixels.Distinct().Count();
            if (colorsInFrame > s_maximumColorsInFrame) s_maximumColorsInFrame = colorsInFrame;
            if (colorsInFrame > 1) s_tornFrameCount++;
        }

        Browser.ReleaseMetalTexture(texture);
        return true;
    }

    private static double Median(List<int> values)
    {
        if (values.Count == 0) return 0;
        var sorted = values.OrderBy(value => value).ToList();
        return sorted[sorted.Count / 2];
    }
}

}
