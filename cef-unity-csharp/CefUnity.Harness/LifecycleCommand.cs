using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Threading;
using CefUnity.Interop;

namespace CefUnity.Harness
{

/// <summary>
///     Unity を使わずに Play/Stop 相当のサイクルを繰り返し、ライフサイクル系の
///     issue を測る診断コマンド。
///
///     <list type="bullet">
///     <item>#8 shutdown が server を回収しない (ゾンビ/孤児化) → サイクル毎に残存プロセス数</item>
///     <item>#9 server が親プロセスの FD (TCP ソケット) を継承する → 事前に LISTEN
///           ソケットを開いておき、外部から lsof で確認できるようにする</item>
///     <item>#10 Mach receive port と surface キャッシュが解放されない → サイクル毎に
///           Mach port 総数・受信ポート番号・キャッシュ数を出す</item>
///     </list>
/// </summary>
internal static class LifecycleCommand
{
    // #9 の確認用。.NET のソケットは既定で FD_CLOEXEC が付くため、そのままでは
    // 子プロセスへ継承されない。Unity (mono) 側のソケットには付いていないという
    // 前提を再現するため、明示的に CLOEXEC を外して spawn 側の挙動を試す。
    [DllImport("libc", SetLastError = true)]
    private static extern int fcntl(int fileDescriptor, int command, int argument);

    private const int F_GETFD = 1;
    private const int F_SETFD = 2;
    private const int FD_CLOEXEC = 1;

    /// <summary>指定 FD の FD_CLOEXEC を外す。成功したら true。</summary>
    private static bool ClearCloseOnExec(int fileDescriptor)
    {
        var flags = fcntl(fileDescriptor, F_GETFD, 0);
        if (flags < 0) return false;
        return fcntl(fileDescriptor, F_SETFD, flags & ~FD_CLOEXEC) == 0;
    }

    private const string PageHtml = """
        <!doctype html><meta charset="utf-8">
        <body style="margin:0;background:#246"><div style="width:100vw;height:100vh"></div></body>
        """;

    public static int Run(int cycleCount, int listenPort, int framesPerCycle = 60)
    {
        var pagePath = Path.Combine(Path.GetTempPath(), "cef_unity_lifecycle.html");
        File.WriteAllText(pagePath, PageHtml);
        var url = new Uri(pagePath).AbsoluteUri;

        // #9: server が継承しうる TCP ソケットを親プロセス側で開いておく。
        TcpListener? listener = null;
        try
        {
            listener = new TcpListener(IPAddress.Loopback, listenPort);
            listener.Start();
            var fileDescriptor = (int)listener.Server.Handle;
            var cleared = ClearCloseOnExec(fileDescriptor);
            Console.WriteLine($"parent_pid={Environment.ProcessId} listening on 127.0.0.1:{listenPort} " +
                              $"fd={fileDescriptor} cloexec_cleared={cleared}");
        }
        catch (SocketException error)
        {
            Console.WriteLine($"listen failed ({error.SocketErrorCode}) — #9 の確認はスキップされます");
        }

        Console.WriteLine($"mach_ports_before_any_init={Browser.DebugMachPortCount()}");

        for (var cycle = 1; cycle <= cycleCount; cycle++)
        {
            CefRuntime.Initialize(useGpu: true, enableLog: true);
            using (var browser = new Browser(1280, 720, url))
            {
                for (var frameIndex = 0; frameIndex < framesPerCycle; frameIndex++)
                {
                    browser.SendExternalBeginFrame((ulong)frameIndex);
                    CefRuntime.Pump();
                    if (Browser.TryReceiveIOSurfaceTexture(out var texture, out _, out _, out _))
                        Browser.ReleaseMetalTexture(texture);
                    Thread.Sleep(16);
                }
            }
            CefRuntime.Shutdown();

            Browser.DebugIOSurfaceState(out var receivePort, out var cacheCount);
            Console.WriteLine(
                $"cycle={cycle,2} mach_ports={Browser.DebugMachPortCount(),5} " +
                $"receive_port={receivePort,6} surface_cache={cacheCount} " +
                $"server_processes={CountServerProcesses()}");
            Thread.Sleep(300);
        }

        Console.WriteLine($"LIFECYCLE_DONE cycles={cycleCount} " +
                          $"mach_ports_final={Browser.DebugMachPortCount()} " +
                          $"server_processes_final={CountServerProcesses()}");
        listener?.Stop();
        return 0;
    }

    /// <summary>残存している cef-unity-server プロセスの数を数える (#8 の確認)。</summary>
    private static int CountServerProcesses()
    {
        try
        {
            using var process = Process.Start(new ProcessStartInfo
            {
                FileName = "/usr/bin/pgrep",
                Arguments = "-f cef-unity-server",
                RedirectStandardOutput = true,
                UseShellExecute = false,
            });
            if (process == null) return -1;
            var output = process.StandardOutput.ReadToEnd();
            process.WaitForExit();
            return output.Split('\n', StringSplitOptions.RemoveEmptyEntries).Length;
        }
        catch (Exception)
        {
            return -1;
        }
    }
}

}
