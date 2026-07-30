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
///     issue #11 が求める「待機パス専用カウンタの分離」もここで行う。現行の Unity 計装は
///     <c>block_avg</c> の分母に fresh+fallback を使うが、この 2 つは抑止スキップパス
///     (ブロックしない) でも加算されるため、1 フレーム当たりの実 spin を過小評価する。
///     本コマンドは spin に入った回数 (<c>wait_entered</c>) を独立に数え、両方の分母で
///     平均を出して差を可視化する。
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
        public int FreshCount;          // Unity の _doublePumpFreshCount 相当
        public int FallbackCount;       // Unity の _doublePumpFallbackCount 相当
        public int IdleSkipCount;       // Unity の _doublePumpIdleCount 相当
        public int SuppressedSkipCount; // Unity には対応カウンタが無い (抑止スキップ)
        public int WaitEnteredCount;    // #11 が求める分離: 実際に spin へ入った回数
        public double SpinTotalMilliseconds;
        public double SpinMaxMilliseconds;
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

                    var activeCount = counters.FreshCount + counters.FallbackCount;
                    var blockAverageOld = activeCount > 0
                        ? counters.SpinTotalMilliseconds / activeCount : 0.0;
                    var blockAverageEntered = counters.WaitEnteredCount > 0
                        ? counters.SpinTotalMilliseconds / counters.WaitEnteredCount : 0.0;
                    var windowMilliseconds = (clock.Elapsed - windowStart).TotalMilliseconds;

                    var line =
                        $"t={clock.Elapsed.TotalSeconds,5:F1}s received={counters.ReceivedCount,3} " +
                        $"max_gap={counters.MaximumGapMilliseconds,6:F1}ms | " +
                        $"fresh={counters.FreshCount,3} fallback={counters.FallbackCount,3} " +
                        $"idle_skip={counters.IdleSkipCount,3} suppressed_skip={counters.SuppressedSkipCount,3} " +
                        $"wait_entered={counters.WaitEnteredCount,3} | " +
                        $"spin_total={counters.SpinTotalMilliseconds,6:F1}ms " +
                        $"({counters.SpinTotalMilliseconds / windowMilliseconds * 100,4:F1}% of wall) " +
                        $"spin_max={counters.SpinMaxMilliseconds,5:F2}ms " +
                        $"block_avg_old={blockAverageOld,5:F2}ms block_avg_entered={blockAverageEntered,5:F2}ms | " +
                        $"delay_0F={counters.ZeroFrameDelayCount,3} delay_1F={counters.OneFrameDelayCount,3} " +
                        $"delay_2F+={counters.LaterFrameDelayCount,3} | " +
                        $"process_cpu={processorMilliseconds,6:F0}ms";
                    Console.WriteLine(line);
                    summaries.Add(line);

                    totals.ReceivedCount += counters.ReceivedCount;
                    totals.FreshCount += counters.FreshCount;
                    totals.FallbackCount += counters.FallbackCount;
                    totals.IdleSkipCount += counters.IdleSkipCount;
                    totals.SuppressedSkipCount += counters.SuppressedSkipCount;
                    totals.WaitEnteredCount += counters.WaitEnteredCount;
                    totals.SpinTotalMilliseconds += counters.SpinTotalMilliseconds;
                    totals.ZeroFrameDelayCount += counters.ZeroFrameDelayCount;
                    totals.OneFrameDelayCount += counters.OneFrameDelayCount;
                    totals.LaterFrameDelayCount += counters.LaterFrameDelayCount;
                    totals.SpinMaxMilliseconds =
                        Math.Max(totals.SpinMaxMilliseconds, counters.SpinMaxMilliseconds);
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
                $"wait_entered_per_second={(totalSeconds > 0 ? (double)totals.WaitEnteredCount / totalSeconds : 0):F1} " +
                $"spin_share={(totalSeconds > 0 ? totals.SpinTotalMilliseconds / (totalSeconds * 1000.0) * 100 : 0):F1}% " +
                $"spin_max={totals.SpinMaxMilliseconds:F2}ms " +
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
    ///     CefUnityBrowserSample.ReceiveBeforeRender の忠実移植。
    ///     カウンタの加算位置も原実装に合わせる (fresh/fallback は抑止スキップでも加算される)。
    /// </summary>
    private static void ReceiveBeforeRender(Browser browser, CefZeroFramePacer pacer,
                                            Stopwatch clock, float zeroFrameWaitMilliseconds,
                                            WindowCounters counters, ulong beginFrameIndexThisFrame)
    {
        if (zeroFrameWaitMilliseconds <= 0f)
        {
            if (Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame)) counters.FreshCount++;
            else counters.FallbackCount++;
            return;
        }

        var blockStart = ElapsedSeconds(clock);

        if (pacer.ShouldSkipAsIdle(inputSentThisFrame: false))
        {
            Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame);
            counters.IdleSkipCount++;
            return;
        }

        if (pacer.ShouldSkipAsSuppressed())
        {
            if (Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame)) counters.FreshCount++;
            else counters.FallbackCount++;
            counters.SuppressedSkipCount++;
            return;
        }

        // ここから先が実際の busy-wait。#11 が数えたい対象。
        counters.WaitEnteredCount++;
        var window = pacer.OpenWaitWindow(zeroFrameWaitMilliseconds);
        while (true)
        {
            var now = ElapsedSeconds(clock);
            if (window.DeadlineReached(now)) break;
            if (window.OnAcceleratedFrameIdSample(now, browser.PeekAcceleratedFrameId())) break;
            Thread.SpinWait(64);
        }

        if (Receive(browser, pacer, clock, counters, beginFrameIndexThisFrame)) counters.FreshCount++;
        else counters.FallbackCount++;

        var spinMilliseconds = (ElapsedSeconds(clock) - blockStart) * 1000.0;
        counters.SpinTotalMilliseconds += spinMilliseconds;
        if (spinMilliseconds > counters.SpinMaxMilliseconds)
            counters.SpinMaxMilliseconds = spinMilliseconds;
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
