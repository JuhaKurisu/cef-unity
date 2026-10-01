using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading;
using CefUnity.Interop;
using CefUnity.Runtime;

namespace CefUnity.Harness
{

/// <summary>
///     クラッシュからの自動復旧を、実際にプロセスを落として確かめる。
///
///     <list type="bullet">
///     <item>renderer: chrome://crash でレンダラーを落とす → 再読み込みで描画が戻る。
///           続けて落とすと、クラッシュループとして 3 回目で再読み込みが止まる</item>
///     <item>server-kill: server を SIGKILL → 再起動・ブラウザの作り直しで描画が戻り、
///           JS で遷移した先の URL が復元される。死んだ server の共有メモリが残らない</item>
///     <item>server-hang: server を SIGSTOP (固まった状態を模す) → heartbeat の停止で検出して復旧</item>
///     <item>gpu-crash: chrome://gpucrash で GPU プロセスを落とし、その後も描画が続くかを観測する</item>
///     </list>
/// </summary>
internal static class CrashRecoveryCommand
{
    private const int Width = 1280;
    private const int Height = 720;

    // 描画が止まっていないことを確かめられるよう、毎フレーム変化するページにする。
    private const string PageHtmlTemplate = """
        <!doctype html><meta charset="utf-8"><title>{0}</title>
        <body style="margin:0;background:#{1}"><div id="counter" style="font:64px monospace;color:#fff">0</div>
        <script>
        let frame = 0;
        (function tick() {{ document.getElementById('counter').textContent = ++frame; requestAnimationFrame(tick); }})();
        </script></body>
        """;

    private static readonly List<string> Events = new List<string>();
    private static bool s_useGpu;

    public static int Run(string scenario, bool useGpu)
    {
        s_useGpu = useGpu;
        var firstUrl = WritePage("a", "246");
        var secondUrl = WritePage("b", "642");

        CefRuntime.ServerLost += status => Record($"ServerLost {status}");
        CefRuntime.ServerRecovered += status => Record($"ServerRecovered {status}");
        CefRuntime.ServerRecoveryFailed += status => Record($"ServerRecoveryFailed {status}");

        CefRuntime.Initialize(useGpu: useGpu, logLevel: CefLogLevel.Verbose);
        var passed = false;
        try
        {
            using var browser = new Browser(Width, Height, firstUrl);
            browser.RenderProcessTerminated += (_, status) => Record($"RenderProcessTerminated {status}");
            browser.Recreated += (_, status) => Record($"Recreated {status}");
            var frames = PumpFrames(browser, 120);
            Console.WriteLine($"initial frames={frames}");
            if (frames == 0)
            {
                Console.Error.WriteLine("FAIL: no frames before the test");
                return 1;
            }

            passed = scenario switch
            {
                "renderer" => RunRenderer(browser, firstUrl),
                "server-kill" => RunServerKill(browser, secondUrl),
                "server-hang" => RunServerHang(browser),
                "gpu-crash" => RunGpuCrash(browser),
                _ => throw new ArgumentException($"unknown scenario: {scenario}")
            };
        }
        finally
        {
            CefRuntime.Shutdown();
        }

        Console.WriteLine(passed ? $"CRASH_RECOVERY_OK {scenario}" : $"CRASH_RECOVERY_FAIL {scenario}");
        return passed ? 0 : 1;
    }

    private static bool RunRenderer(Browser browser, string pageUrl)
    {
        var passed = true;
        // 1 回目は CEF のデバッグ URL で落とす (LoadUrl はクラッシュ判定の履歴を消すので
        // ループ判定には使えない)。2 回目以降は利用者の操作なしに落ち続ける状況を模して、
        // レンダラープロセスを外から落とす。
        for (var crashIndex = 1; crashIndex <= 3; crashIndex++)
        {
            if (crashIndex == 1)
                browser.LoadUrl("chrome://crash");
            else
                foreach (var renderer in ProcessControl.FindRendererProcessIdentifiers()) ProcessControl.Crash(renderer);
            var terminated = WaitFor(browser, () => browser.GetRecoveryStatus().RenderProcessTerminationCount >= crashIndex, 10);
            var status = browser.GetRecoveryStatus();
            var frames = PumpFrames(browser, 120);
            var url = browser.GetUrl();
            Console.WriteLine($"crash #{crashIndex}: terminated={terminated} {status} frames after={frames} url={url}");
            passed &= terminated;
            if (crashIndex < 3)
                passed &= !status.RenderProcessReloadSuppressed && frames > 0 && url == pageUrl;
            else
                passed &= status.RenderProcessReloadSuppressed && frames == 0; // 3 回目はクラッシュループとして諦める
        }

        // 利用者が明示的に開き直せば戻る。
        browser.LoadUrl(pageUrl);
        var framesAfterLoad = PumpFrames(browser, 120);
        var statusAfterLoad = browser.GetRecoveryStatus();
        Console.WriteLine($"after LoadUrl: frames={framesAfterLoad} {statusAfterLoad}");
        passed &= framesAfterLoad > 0 && !statusAfterLoad.RenderProcessReloadSuppressed;
        passed &= Events.Count(entry => entry.StartsWith("RenderProcessTerminated")) == 3;
        return passed;
    }

    private static bool RunServerKill(Browser browser, string secondUrl)
    {
        // クライアントの知らない遷移 (ページ内の JS) で移った先が復元されることを見る。
        browser.ExecuteJavaScriptBlocking($"location.href = '{secondUrl}';");
        PumpFrames(browser, 120);
        Console.WriteLine($"url before kill={browser.GetUrl()}");

        var serverProcessIdentifier = ProcessControl.FindServerProcessIdentifier();
        Console.WriteLine($"killing server pid={serverProcessIdentifier}");
        ProcessControl.Kill(serverProcessIdentifier);

        var recovered = WaitFor(browser, () => CefRuntime.GetServerStatus().RecoveryCount >= 1, 30);
        Console.WriteLine($"recovered={recovered} status={CefRuntime.GetServerStatus()}");
        if (!recovered) return false;

        var frames = PumpFrames(browser, 180);
        var url = browser.GetUrl();
        var newServerProcessIdentifier = ProcessControl.FindServerProcessIdentifier();
        var leftovers = Directory.GetFiles(Path.GetTempPath(), $"cef-unity-*-{serverProcessIdentifier}*");
        Console.WriteLine($"frames after recovery={frames} url={url} new_server_pid={newServerProcessIdentifier} " +
                          $"leftover_shared_memory={leftovers.Length}");
        foreach (var leftover in leftovers) Console.WriteLine($"  leftover: {leftover}");
        return frames > 0 && url == secondUrl && newServerProcessIdentifier != serverProcessIdentifier &&
               leftovers.Length == 0 && Events.Any(entry => entry.StartsWith("ServerLost Recovering (lastLossReason=Exited"));
    }

    private static bool RunServerHang(Browser browser)
    {
        var serverProcessIdentifier = ProcessControl.FindServerProcessIdentifier();
        Console.WriteLine($"stopping server pid={serverProcessIdentifier}");
        var stoppedAt = Stopwatch.StartNew();
        ProcessControl.Suspend(serverProcessIdentifier);

        var recovered = WaitFor(browser, () => CefRuntime.GetServerStatus().RecoveryCount >= 1, 40);
        Console.WriteLine($"recovered={recovered} after {stoppedAt.Elapsed.TotalSeconds:F1}s " +
                          $"status={CefRuntime.GetServerStatus()}");
        if (!recovered) return false;
        var frames = PumpFrames(browser, 180);
        Console.WriteLine($"frames after recovery={frames}");
        return frames > 0 && Events.Any(entry => entry.Contains("lastLossReason=Unresponsive"));
    }

    /// <summary>
    ///     GPU プロセスが落ちると既存ブラウザの描画が止まる (CEF の挙動)。server が
    ///     合成経路の停止を検出してブラウザを作り直し、描画が戻ることを確かめる。
    ///     静止ページで作り直しが誤って起きないことも確かめる。
    /// </summary>
    private static bool RunGpuCrash(Browser browser)
    {
        var passed = true;
        // 静止ページ (描画が止まって当然の状態) で誤って作り直さないこと。
        var staticUrl = WriteStaticPage();
        browser.LoadUrl(staticUrl);
        PumpFrames(browser, 12 * 60);
        var idleStatus = browser.GetRecoveryStatus();
        Console.WriteLine($"static page for 12s: {idleStatus}");
        passed &= idleStatus.RecreationCount == 0;

        for (var crashIndex = 1; crashIndex <= 2; crashIndex++)
        {
            browser.LoadUrl("chrome://gpucrash");
            var recreated = WaitFor(browser, () => browser.GetRecoveryStatus().RecreationCount >= crashIndex, 20);
            var frames = PumpFrames(browser, 120);
            var url = browser.GetUrl();
            Console.WriteLine($"gpucrash #{crashIndex}: recreated={recreated} frames after={frames} url={url} " +
                              $"{browser.GetRecoveryStatus()} server={CefRuntime.GetServerStatus()}");
            passed &= recreated && frames > 0 && url == staticUrl;
        }
        passed &= Events.Count(entry => entry.StartsWith("Recreated")) == 2;
        return passed;
    }

    private static string WriteStaticPage()
    {
        var path = Path.Combine(Path.GetTempPath(), "cef_unity_crash_recovery_static.html");
        File.WriteAllText(path, "<!doctype html><meta charset=\"utf-8\"><body style=\"background:#264\">static</body>");
        return new Uri(path).AbsoluteUri;
    }

    private static string WritePage(string name, string color)
    {
        var path = Path.Combine(Path.GetTempPath(), $"cef_unity_crash_recovery_{name}.html");
        File.WriteAllText(path, string.Format(PageHtmlTemplate, name, color));
        return new Uri(path).AbsoluteUri;
    }

    private static void Record(string entry)
    {
        Events.Add(entry);
        Console.WriteLine($"  event: {entry}");
    }

    private static ulong s_observedAcceleratedFrameId;

    /// <summary>1 フレームぶん Unity と同じ手順を回し、新しいフレームを受け取れたら true。</summary>
    private static bool PumpOnce(Browser browser, ulong frameIndex)
    {
        browser.SendExternalBeginFrame(frameIndex);
        CefRuntime.Pump();
        Thread.Sleep(16);
        if (!s_useGpu) return browser.TryGetBuffer(out _, out _, out _);
        if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
        {
            // macOS は Mach 経由でテクスチャを実際に受け取る (server 再起動後の再接続も確かめる)。
            if (!Browser.TryReceiveIOSurfaceTexture(out var texture, out _, out _, out _)) return false;
            Browser.ReleaseMetalTexture(texture);
            return true;
        }
        // Windows の共有テクスチャを開くには D3D11/D3D12 の device が要る。harness は持たないので、
        // server が共有メモリに公開する accelerated paint の通し番号の増分で数える。
        var acceleratedFrameId = browser.PeekAcceleratedFrameId();
        if (acceleratedFrameId == s_observedAcceleratedFrameId) return false;
        s_observedAcceleratedFrameId = acceleratedFrameId;
        return true;
    }

    private static ulong s_frameIndex;

    private static int PumpFrames(Browser browser, int frameCount)
    {
        var received = 0;
        for (var index = 0; index < frameCount; index++)
            if (PumpOnce(browser, ++s_frameIndex)) received++;
        return received;
    }

    /// <summary>GPU モードでも software paint に落ちていないかを見るため、共有メモリの CPU バッファを数える。</summary>
    private static int CountSoftwareFrames(Browser browser, int frameCount)
    {
        var received = 0;
        for (var index = 0; index < frameCount; index++)
        {
            browser.SendExternalBeginFrame(++s_frameIndex);
            CefRuntime.Pump();
            Thread.Sleep(16);
            if (browser.TryGetBuffer(out _, out _, out _)) received++;
        }
        return received;
    }

    private static bool WaitFor(Browser browser, Func<bool> condition, int timeoutSeconds)
    {
        var stopwatch = Stopwatch.StartNew();
        while (stopwatch.Elapsed.TotalSeconds < timeoutSeconds)
        {
            if (condition()) return true;
            PumpOnce(browser, ++s_frameIndex);
        }
        return condition();
    }

}
}
