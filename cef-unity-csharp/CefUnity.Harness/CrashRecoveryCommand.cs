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

        CefRuntime.Initialize(useGpu: useGpu, enableLog: true);
        var passed = false;
        try
        {
            using var browser = new Browser(Width, Height, firstUrl);
            browser.RenderProcessTerminated += (_, status) => Record($"RenderProcessTerminated {status}");
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
        // レンダラープロセスへ直接 SIGSEGV を送る。
        for (var crashIndex = 1; crashIndex <= 3; crashIndex++)
        {
            if (crashIndex == 1)
                browser.LoadUrl("chrome://crash");
            else
                foreach (var renderer in FindRendererProcessIdentifiers()) Signal("SEGV", renderer);
            var terminated = WaitFor(browser, () => browser.GetRenderProcessStatus().TerminationCount >= crashIndex, 10);
            var status = browser.GetRenderProcessStatus();
            var frames = PumpFrames(browser, 120);
            var url = browser.GetUrl();
            Console.WriteLine($"crash #{crashIndex}: terminated={terminated} {status} frames after={frames} url={url}");
            passed &= terminated;
            if (crashIndex < 3)
                passed &= !status.ReloadSuppressed && frames > 0 && url == pageUrl;
            else
                passed &= status.ReloadSuppressed && frames == 0; // 3 回目はクラッシュループとして諦める
        }

        // 利用者が明示的に開き直せば戻る。
        browser.LoadUrl(pageUrl);
        var framesAfterLoad = PumpFrames(browser, 120);
        var statusAfterLoad = browser.GetRenderProcessStatus();
        Console.WriteLine($"after LoadUrl: frames={framesAfterLoad} {statusAfterLoad}");
        passed &= framesAfterLoad > 0 && !statusAfterLoad.ReloadSuppressed;
        passed &= Events.Count(entry => entry.StartsWith("RenderProcessTerminated")) == 3;
        return passed;
    }

    private static bool RunServerKill(Browser browser, string secondUrl)
    {
        // クライアントの知らない遷移 (ページ内の JS) で移った先が復元されることを見る。
        browser.ExecuteJavaScriptBlocking($"location.href = '{secondUrl}';");
        PumpFrames(browser, 120);
        Console.WriteLine($"url before kill={browser.GetUrl()}");

        var serverProcessIdentifier = FindServerProcessIdentifier();
        Console.WriteLine($"killing server pid={serverProcessIdentifier}");
        Signal("KILL", serverProcessIdentifier);

        var recovered = WaitFor(browser, () => CefRuntime.GetServerStatus().RecoveryCount >= 1, 30);
        Console.WriteLine($"recovered={recovered} status={CefRuntime.GetServerStatus()}");
        if (!recovered) return false;

        var frames = PumpFrames(browser, 180);
        var url = browser.GetUrl();
        var newServerProcessIdentifier = FindServerProcessIdentifier();
        var leftovers = Directory.GetFiles(Path.GetTempPath(), $"cef-unity-*-{serverProcessIdentifier}*");
        Console.WriteLine($"frames after recovery={frames} url={url} new_server_pid={newServerProcessIdentifier} " +
                          $"leftover_shared_memory={leftovers.Length}");
        foreach (var leftover in leftovers) Console.WriteLine($"  leftover: {leftover}");
        return frames > 0 && url == secondUrl && newServerProcessIdentifier != serverProcessIdentifier &&
               leftovers.Length == 0 && Events.Any(entry => entry.StartsWith("ServerLost Recovering (lastLossReason=Exited"));
    }

    private static bool RunServerHang(Browser browser)
    {
        var serverProcessIdentifier = FindServerProcessIdentifier();
        Console.WriteLine($"stopping server pid={serverProcessIdentifier}");
        var stoppedAt = Stopwatch.StartNew();
        Signal("STOP", serverProcessIdentifier);

        var recovered = WaitFor(browser, () => CefRuntime.GetServerStatus().RecoveryCount >= 1, 40);
        Console.WriteLine($"recovered={recovered} after {stoppedAt.Elapsed.TotalSeconds:F1}s " +
                          $"status={CefRuntime.GetServerStatus()}");
        if (!recovered) return false;
        var frames = PumpFrames(browser, 180);
        Console.WriteLine($"frames after recovery={frames}");
        return frames > 0 && Events.Any(entry => entry.Contains("lastLossReason=Unresponsive"));
    }

    private static bool RunGpuCrash(Browser browser)
    {
        browser.LoadUrl("chrome://gpucrash");
        Console.WriteLine($"after gpucrash: frames={PumpFrames(browser, 240)}");
        browser.Resize(Width + 1, Height);
        Console.WriteLine($"after resize: frames={PumpFrames(browser, 120)}");
        browser.Resize(Width, Height);
        Console.WriteLine($"after resize back: frames={PumpFrames(browser, 120)}");
        var url = browser.GetUrl();
        browser.LoadUrl(url);
        Console.WriteLine($"after reload: frames={PumpFrames(browser, 240)}");
        using (var second = new Browser(Width, Height, url))
        {
            Console.WriteLine($"new browser in the same server: frames={PumpFrames(second, 240)}");
        }
        return true;
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

    /// <summary>1 フレームぶん Unity と同じ手順を回し、新しいフレームを受け取れたら true。</summary>
    private static bool PumpOnce(Browser browser, ulong frameIndex)
    {
        browser.SendExternalBeginFrame(frameIndex);
        CefRuntime.Pump();
        Thread.Sleep(16);
        if (!s_useGpu) return browser.TryGetBuffer(out _, out _, out _);
        if (!Browser.TryReceiveIOSurfaceTexture(out var texture, out _, out _, out _)) return false;
        Browser.ReleaseMetalTexture(texture);
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

    /// <summary>このプロセスが起動した server (helper はその子なので含まれない)。</summary>
    private static int FindServerProcessIdentifier()
    {
        using var process = Process.Start(new ProcessStartInfo
        {
            FileName = "/usr/bin/pgrep",
            Arguments = $"-P {Environment.ProcessId} -f cef-unity-server",
            RedirectStandardOutput = true,
            UseShellExecute = false,
        })!;
        var output = process.StandardOutput.ReadToEnd();
        process.WaitForExit();
        return output.Split('\n', StringSplitOptions.RemoveEmptyEntries).Select(int.Parse).Single();
    }

    /// <summary>
    ///     server の子のうち、レンダラープロセス。Chromium は次のナビゲーション用に予備の
    ///     レンダラーを持つことがあるので複数返り得る (予備を落としてもブラウザには影響しない)。
    /// </summary>
    private static int[] FindRendererProcessIdentifiers()
    {
        using var process = Process.Start(new ProcessStartInfo
        {
            FileName = "/usr/bin/pgrep",
            Arguments = $"-P {FindServerProcessIdentifier()} -f type=renderer",
            RedirectStandardOutput = true,
            UseShellExecute = false,
        })!;
        var output = process.StandardOutput.ReadToEnd();
        process.WaitForExit();
        return output.Split('\n', StringSplitOptions.RemoveEmptyEntries).Select(int.Parse).ToArray();
    }

    private static void Signal(string signal, int processIdentifier)
    {
        using var process = Process.Start("/bin/kill", $"-{signal} {processIdentifier}");
        process.WaitForExit();
    }
}
}
