using System;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;

namespace CefUnity.Harness
{

/// <summary>
///     クラッシュ試験で server とその子プロセスを探して落とす。macOS・Linux は pgrep / kill、
///     Windows は Win32_Process と Process.Kill / NtSuspendProcess を使う。
/// </summary>
internal static class ProcessControl
{
    private static bool IsWindows => RuntimeInformation.IsOSPlatform(OSPlatform.Windows);

    /// <summary>このプロセスが起動した server (helper はその子なので含まれない)。</summary>
    public static int FindServerProcessIdentifier() =>
        FindChildren(Environment.ProcessId, IsWindows ? "cef-unity-server" : "cef-unity-server").Single();

    /// <summary>
    ///     server の子のうち、レンダラープロセス。Chromium は次のナビゲーション用に予備の
    ///     レンダラーを持つことがあるので複数返り得る (予備を落としてもブラウザには影響しない)。
    /// </summary>
    public static int[] FindRendererProcessIdentifiers() =>
        FindChildren(FindServerProcessIdentifier(), "--type=renderer");

    /// <summary>
    ///     レンダラーをクラッシュさせる。Unix は SIGSEGV。Windows には相当する外部からの
    ///     手段が無いので強制終了する (終了理由は「kill された」になるが、復旧の経路は同じ)。
    /// </summary>
    public static void Crash(int processIdentifier)
    {
        if (IsWindows) Kill(processIdentifier);
        else RunUnixKill("SEGV", processIdentifier);
    }

    public static void Kill(int processIdentifier)
    {
        if (IsWindows) Process.GetProcessById(processIdentifier).Kill();
        else RunUnixKill("KILL", processIdentifier);
    }

    /// <summary>プロセスの全スレッドを止める (固まった状態を模す)。</summary>
    public static void Suspend(int processIdentifier)
    {
        if (!IsWindows)
        {
            RunUnixKill("STOP", processIdentifier);
            return;
        }
        using var process = Process.GetProcessById(processIdentifier);
        var status = NtSuspendProcess(process.Handle);
        if (status != 0) throw new InvalidOperationException($"NtSuspendProcess failed: 0x{status:x}");
    }

    [DllImport("ntdll.dll")]
    private static extern int NtSuspendProcess(IntPtr processHandle);

    /// <summary><paramref name="parentProcessIdentifier" /> の子のうち、コマンドラインに <paramref name="pattern" /> を含むもの。</summary>
    private static int[] FindChildren(int parentProcessIdentifier, string pattern)
    {
        var startInfo = IsWindows
            ? new ProcessStartInfo
            {
                FileName = "powershell",
                Arguments = "-NoProfile -NonInteractive -Command \"Get-CimInstance Win32_Process -Filter " +
                            $"'ParentProcessId={parentProcessIdentifier}' | ForEach-Object {{ " +
                            "[string]$_.ProcessId + ' ' + $_.CommandLine }\"",
            }
            : new ProcessStartInfo
            {
                FileName = "/usr/bin/pgrep",
                Arguments = $"-P {parentProcessIdentifier} -f -l -a .",
            };
        startInfo.RedirectStandardOutput = true;
        startInfo.UseShellExecute = false;
        using var process = Process.Start(startInfo)!;
        var output = process.StandardOutput.ReadToEnd();
        process.WaitForExit();
        return output.Split('\n', StringSplitOptions.RemoveEmptyEntries)
            .Select(line => line.Trim())
            .Where(line => line.Contains(pattern))
            .Select(line => int.Parse(line.Split(' ')[0]))
            .ToArray();
    }

    private static void RunUnixKill(string signal, int processIdentifier)
    {
        using var process = Process.Start("/bin/kill", $"-{signal} {processIdentifier}");
        process.WaitForExit();
    }
}
}
