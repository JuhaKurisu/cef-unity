using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using CefUnity.Interop;
using CefUnity.Runtime;

namespace CefUnity.Harness
{

/// <summary>
///     Unity を使わずに issue #11 (メインスレッドの paint 待ち busy-wait) を再現する診断コマンド。
///
///     <para>
///     <c>CefUnityBrowserSample.ReceiveBeforeRender</c> の待機ループを忠実に移植する。
///     判定は Unity 側と同一の <see cref="CefZeroFramePacer" /> (純 C#) をそのまま使うため、
///     ステートマシンの挙動は Unity と同じになる。
///     </para>
///     <para>
///     issue #11 が求める「待機パス専用カウンタの分離」は <see cref="CefZeroFrameWaitStatistics" />
///     として Core 側で恒久化した。本コマンドは Unity と同じそのクラスへ集計を委譲するので、
///     カウンタの加算位置は Unity と常に一致する。
///     </para>
///     <para>
///     Unity との差: Unity では BF#1 (EarlyUpdate) と recv (PostLateUpdate) の間にゲーム処理と
///     描画が入るため待ち窓が食われる。ここでは間に何も無いので窓を最大まで使う = spin の
///     最悪ケースを測る。
///     </para>
/// </summary>
internal static class ZeroFrameWaitCommand
{
    /// <summary>rAF で毎フレーム全画面を塗り替えるページ (入力なしの連続アニメーション条件)。</summary>
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

    private sealed class WindowCounters
    {
        /// <summary>Unity と共有する集計 (受信の成否・パス内訳・spin)。</summary>
        public readonly CefZeroFrameWaitStatistics Statistics = new CefZeroFrameWaitStatistics();
        public int ReceivedCount;
        public double MaximumGapMilliseconds;
        // 待機の目的側: 受け取った paint が何フレーム前の BeginFrame 由来か (0 = 同フレーム = 0F)。
        public int ZeroFrameDelayCount;   // 遅延 0 フレーム
        public int OneFrameDelayCount;    // 遅延 1 フレーム
        public int LaterFrameDelayCount;  // 遅延 2 フレーム以上
    }

    /// <summary>
    ///     5Hz でのみ全画面を塗り替えるページ。0F 待ちが本来価値を発揮するとされる
    ///     「間欠 damage」条件 (server 側の damage streak 抑止に入らないため flush が出る)。
    /// </summary>
    private const string IntermittentHtml = """
        <!doctype html><meta charset="utf-8">
        <body style="margin:0;overflow:hidden">
        <div id="surface" style="width:100vw;height:100vh"></div>
        <script>
        let step = 0;
        const surface = document.getElementById('surface');
        setInterval(function () {
            step = (step + 1) % 256;
            surface.style.background =
                'rgb(' + step + ',' + ((step * 7) % 256) + ',' + ((step * 13) % 256) + ')';
        }, 200);
        </script>
        </body>
        """;

    public static int Run(int durationSeconds, float zeroFrameWaitMilliseconds,
                          int viewportWidth = 1920, int viewportHeight = 1080,
                          string pageMode = "animation")
    {
        var intermittent = pageMode == "intermittent";
        var pagePath = Path.Combine(Path.GetTempPath(),
            intermittent ? "cef_unity_zero_frame_wait_5hz.html" : "cef_unity_zero_frame_wait.html");
        File.WriteAllText(pagePath, intermittent ? IntermittentHtml : AnimationHtml);
        var url = new Uri(pagePath).AbsoluteUri;

        CefRuntime.Initialize(useGpu: true, enableLog: true);
        try
        {
            using var browser = new Browser(viewportWidth, viewportHeight, url);
            Console.WriteLine(
                $"viewport={viewportWidth}x{viewportHeight} " +
                $"zero_frame_wait={zeroFrameWaitMilliseconds:F1}ms " +
                $"({(zeroFrameWaitMilliseconds > 0f ? "busy-wait ON" : "busy-wait OFF")}) " +
                $"page={pageMode}");

            var pacer = new CefZeroFramePacer();
            var clock = Stopwatch.StartNew();
            var beginFrameIndex = 0UL;

            // ページのロードと GPU 経路の接続を待つ。
            for (var warmupIndex = 0; warmupIndex < 120; warmupIndex++)
            {
                browser.SendExternalBeginFrame(beginFrameIndex++);
                CefRuntime.Pump();
                if (TryReceive()) pacer.OnFreshPaint(); else pacer.OnNoPaint();
                Thread.Sleep(16);
            }
            Console.WriteLine($"iosurface_connected={Browser.IsIOSurfaceConnected()}");

            var frameInterval = TimeSpan.FromTicks(TimeSpan.TicksPerSecond / 60);
            var deadline = clock.Elapsed;
            var windowStart = clock.Elapsed;
            var counters = new WindowCounters();
            var process = Process.GetCurrentProcess();
            var processorTimeAtWindowStart = process.TotalProcessorTime;
            var summaries = new List<string>();
            var totals = new WindowCounters();
            var totalSeconds = 0;

            while (clock.Elapsed < TimeSpan.FromSeconds(durationSeconds))
            {
                deadline += frameInterval;

                // Unity: EarlyUpdate 末尾で OnBeginFrame → BF#1 送信。入力なし条件で回す。
                pacer.OnBeginFrame(ElapsedSeconds(clock), browser.PeekAcceleratedFrameId(),
                                   inputSentThisFrame: false);
                var beginFrameIndexThisFrame = beginFrameIndex;
                browser.SendExternalBeginFrame(beginFrameIndex++);
                CefRuntime.Pump();

                // Unity: PostLateUpdate の ReceiveBeforeRender 相当。
                ReceiveBeforeRender(browser, pacer, clock, zeroFrameWaitMilliseconds, counters,
                                    beginFrameIndexThisFrame);

                if (clock.Elapsed - windowStart >= TimeSpan.FromSeconds(1))
                {
                    var processorTime = process.TotalProcessorTime;
                    var processorMilliseconds =
                        (processorTime - processorTimeAtWindowStart).TotalMilliseconds;
                    processorTimeAtWindowStart = processorTime;

                    var windowMilliseconds = (clock.Elapsed - windowStart).TotalMilliseconds;
                    var statistics = counters.Statistics;

                    var line =
                        $"t={clock.Elapsed.TotalSeconds,5:F1}s received={counters.ReceivedCount,3} " +
                        $"max_gap={counters.MaximumGapMilliseconds,6:F1}ms | " +
                        $"{statistics.FormatLine()} | " +
                        $"spin_total={statistics.SpinTotalMilliseconds,6:F1}ms " +
                        $"({statistics.SpinTotalMilliseconds / windowMilliseconds * 100,4:F1}% of wall) | " +
                        $"delay_0F={counters.ZeroFrameDelayCount,3} delay_1F={counters.OneFrameDelayCount,3} " +
                        $"delay_2F+={counters.LaterFrameDelayCount,3} | " +
                        $"process_cpu={processorMilliseconds,6:F0}ms";
                    Console.WriteLine(line);
                    summaries.Add(line);

                    totals.Statistics.Add(statistics);
                    totals.ReceivedCount += counters.ReceivedCount;
                    totals.ZeroFrameDelayCount += counters.ZeroFrameDelayCount;
                    totals.OneFrameDelayCount += counters.OneFrameDelayCount;
                    totals.LaterFrameDelayCount += counters.LaterFrameDelayCount;
                    totals.MaximumGapMilliseconds =
                        Math.Max(totals.MaximumGapMilliseconds, counters.MaximumGapMilliseconds);
                    totalSeconds++;

                    counters = new WindowCounters();
                    windowStart = clock.Elapsed;
                }

                var remaining = deadline - clock.Elapsed;
                if (remaining > TimeSpan.Zero) Thread.Sleep(remaining);
                else deadline = clock.Elapsed;
            }

            Console.WriteLine();
            Console.WriteLine(
                $"CLIENT_SUMMARY zero_frame_wait={zeroFrameWaitMilliseconds:F1}ms seconds={totalSeconds} " +
                $"received_per_second={(totalSeconds > 0 ? (double)totals.ReceivedCount / totalSeconds : 0):F1} " +
                $"gap_max={totals.MaximumGapMilliseconds:F1}ms " +
                $"wait_entered_per_second={(totalSeconds > 0 ? (double)totals.Statistics.WaitEnteredCount / totalSeconds : 0):F1} " +
                $"spin_share={(totalSeconds > 0 ? totals.Statistics.SpinTotalMilliseconds / (totalSeconds * 1000.0) * 100 : 0):F1}% " +
                $"spin_max={totals.Statistics.SpinMaximumMilliseconds:F2}ms " +
                $"block_avg_entered={totals.Statistics.BlockAverageMilliseconds:F2}ms " +
                $"delay_0F={totals.ZeroFrameDelayCount} delay_1F={totals.OneFrameDelayCount} " +
                $"delay_2F+={totals.LaterFrameDelayCount} " +
                $"zero_frame_share={(totals.ReceivedCount > 0 ? (double)totals.ZeroFrameDelayCount / totals.ReceivedCount * 100 : 0):F1}%");

            Console.WriteLine();
            foreach (var line in CefRuntime.GetLogs().Where(line => line.Contains("STATISTICS")))
                Console.WriteLine($"SERVER {line}");

            return totalSeconds > 0 ? 0 : 1;
        }
        finally
        {
            CefRuntime.Shutdown();
        }
    }

    /// <summary>
    ///     CefUnityBrowserSample.ReceiveBeforeRender の忠実移植。集計は Unity と同じ
    ///     CefZeroFrameWaitStatistics に委譲するため、カウンタの加算位置も自動的に一致する。
    /// </summary>
    private static void ReceiveBeforeRender(Browser browser, CefZeroFramePacer pacer,
                                            Stopwatch clock, float zeroFrameWaitMilliseconds,
                                            WindowCounters counters, ulong beginFrameIndexThisFrame)
    {
        if (zeroFrameWaitMilliseconds <= 0f)
        {
            counters.Statistics.RecordNoWaitReceive(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        if (pacer.ShouldSkipAsIdle(inputSentThisFrame: false))
        {
            counters.Statistics.RecordIdleSkip(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        if (pacer.ShouldSkipAsSuppressed())
        {
            counters.Statistics.RecordSuppressedSkip(
                Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame));
            return;
        }

        // ここから先が実際の busy-wait。#11 が数えたい対象。
        var blockStart = ElapsedSeconds(clock);
        var window = pacer.OpenWaitWindow(zeroFrameWaitMilliseconds);
        while (true)
        {
            var now = ElapsedSeconds(clock);
            if (window.DeadlineReached(now)) break;
            if (window.OnAcceleratedFrameIdSample(now, browser.PeekAcceleratedFrameId())) break;
            Thread.SpinWait(64);
        }

        var receivedAfterWait = Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame);
        counters.Statistics.RecordWaitCompleted(
            receivedAfterWait, (ElapsedSeconds(clock) - blockStart) * 1000.0);
    }

    private static double s_lastReceivedSeconds = -1;

    private static bool Receive(Browser browser, CefZeroFramePacer pacer, Stopwatch clock,
                                WindowCounters counters, ulong beginFrameIndexThisFrame)
    {
        if (TryReceive())
        {
            pacer.OnFreshPaint();
            counters.ReceivedCount++;

            // 受け取った paint が何フレーム前の BeginFrame 由来かを数える (0F 待ちの目的側)。
            var paintBeginFrameIndex = browser.GetAcceleratedPaintUnityFrame();
            var delayFrames = beginFrameIndexThisFrame >= paintBeginFrameIndex
                ? beginFrameIndexThisFrame - paintBeginFrameIndex
                : 0;
            if (delayFrames == 0) counters.ZeroFrameDelayCount++;
            else if (delayFrames == 1) counters.OneFrameDelayCount++;
            else counters.LaterFrameDelayCount++;

            var nowSeconds = clock.Elapsed.TotalMilliseconds;
            if (s_lastReceivedSeconds >= 0)
            {
                var gap = nowSeconds - s_lastReceivedSeconds;
                if (gap > counters.MaximumGapMilliseconds) counters.MaximumGapMilliseconds = gap;
            }
            s_lastReceivedSeconds = nowSeconds;
            return true;
        }
        pacer.OnNoPaint();
        return false;
    }

    private static bool TryReceive()
    {
        if (!Browser.TryReceiveIOSurfaceTexture(out var texture, out _, out _, out _)) return false;
        Browser.ReleaseMetalTexture(texture);
        return true;
    }

    /// <summary>Unity の Time.realtimeSinceStartup と同じ「秒の float」に揃える。</summary>
    private static float ElapsedSeconds(Stopwatch clock) => (float)clock.Elapsed.TotalSeconds;
}

}
