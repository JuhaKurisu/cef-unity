using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using CefUnity.Interop;
using CefUnity.Runtime;
using CefUnity.Viewer;

MacMomentumScrollSupport.Enable();

if (args.Length > 0 && args[0] == "spike") return SpikeRunner.Run();

var viewerOptions = ViewerOptions.Parse(args);
if (viewerOptions == null)
{
    Console.Error.WriteLine(ViewerOptions.Usage);
    return 2;
}

if (viewerOptions.AnalyzePath != null)
{
    var sentDeltaY = new List<int>();
    foreach (var line in File.ReadLines(viewerOptions.AnalyzePath).Skip(1))
    {
        var columns = line.Split(',');
        if (columns.Length >= 5) sentDeltaY.Add(int.Parse(columns[4]));
    }
    var roughness = ScrollRoughnessAnalyzer.ComputeRoughness(sentDeltaY);
    Console.WriteLine(FormattableString.Invariant($"frames={sentDeltaY.Count} roughness={roughness:F4}"));
    return 0;
}

CefRuntime.Initialize(useGpu: true);
try
{
    using var frameSource = new CefFrameSource(viewerOptions.Width, viewerOptions.Height, viewerOptions.Url);
    using var scrollMatrix = new ScrollInputMatrix();
    CefUnity.Runtime.ScrollReplaySource? replaySource = null;
    if (viewerOptions.ReplayPath != null)
    {
        replaySource = new ScrollReplaySource(File.ReadLines(viewerOptions.ReplayPath));
        if (replaySource.TotalEvents == 0)
        {
            Console.Error.WriteLine($"replay: {viewerOptions.ReplayPath} に over=1 の E 行がない");
            return 2;
        }
        Console.WriteLine($"replay: {replaySource.TotalEvents} events");
        scrollMatrix.AttachSource(replaySource);
    }
    using var statistics = viewerOptions.StatisticsPath != null
        ? new StatisticsRecorder(viewerOptions.StatisticsPath) : null;
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource, scrollMatrix, statistics, replaySource);
    viewerWindow.Run();
}
catch (Exception exception)
{
    Console.Error.WriteLine($"FATAL: {exception}");
    foreach (var line in CefRuntime.GetLogs()) Console.Error.WriteLine($"[cef] {line}");
    Console.Error.WriteLine("復旧手順: サーバー残留は `pkill -f cef-unity-server`、起動ハングはキャッシュ破損の可能性 → $TMPDIR の cef_unity_cache を削除");
    return 1;
}
finally
{
    CefRuntime.Shutdown();
}
return 0;
