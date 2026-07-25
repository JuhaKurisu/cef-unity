using CefUnity.Interop;
using CefUnity.Viewer;

if (args.Length > 0 && args[0] == "spike") return SpikeRunner.Run();

var viewerOptions = ViewerOptions.Parse(args);
if (viewerOptions == null)
{
    Console.Error.WriteLine(ViewerOptions.Usage);
    return 2;
}

CefRuntime.Initialize(useGpu: true);
try
{
    using var frameSource = new CefFrameSource(viewerOptions.Width, viewerOptions.Height, viewerOptions.Url);
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource);
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
