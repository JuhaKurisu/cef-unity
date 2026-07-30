using CefUnity.Interop;

// サブコマンド: (なし)=スモーク, dump=1 フレームを PNG 保存, replay=Phase 4 で追加
var command = args.Length > 0 ? args[0] : "smoke";
if (command == "smoke")
{
    var frames = 0;
    CefRuntime.Initialize(useGpu: false);
    using (var browser = new Browser(1280, 720, "https://example.com"))
    {
        for (var frameIndex = 0; frameIndex < 600; frameIndex++)
        {
            browser.SendExternalBeginFrame((ulong)frameIndex);
            CefRuntime.Pump();
            Thread.Sleep(16);
            if (browser.TryGetBuffer(out var bgra, out var width, out var height))
            {
                frames++;
                if (frames <= 3 || frames % 25 == 0)
                    Console.WriteLine($"Frame #{frames}: {width}x{height}, {bgra.Length} bytes");
            }
        }
        Console.WriteLine($"SMOKE_OK frames={frames}");
    }
    CefRuntime.Shutdown();
    return frames > 0 ? 0 : 1;
}
if (command == "dump")
{
    var outputPath = args.Length > 1 ? args[1] : "frame.png";
    CefRuntime.Initialize(useGpu: false);
    var written = false;
    using (var browser = new Browser(1280, 720, "https://example.com"))
    {
        for (var frameIndex = 0; frameIndex < 600 && !written; frameIndex++)
        {
            browser.SendExternalBeginFrame((ulong)frameIndex);
            CefRuntime.Pump();
            Thread.Sleep(16);
            // 最初の 120 フレームはページのロード待ちに使い、白紙を掴まないようにする
            if (frameIndex < 120) continue;
            if (browser.TryGetBuffer(out var bgra, out var width, out var height))
            {
                CefUnity.Harness.PortableNetworkGraphicsWriter.WriteBgra(outputPath, bgra.ToArray(), width, height);
                Console.WriteLine($"DUMP_OK {outputPath} {width}x{height}");
                written = true;
            }
        }
    }
    CefRuntime.Shutdown();
    if (!written) Console.Error.WriteLine("DUMP FAIL: no frame captured");
    return written ? 0 : 1;
}
if (command == "paint-statistics")
{
    // usage: paint-statistics [seconds] [width] [height] [animation|small-damage]
    var durationSeconds = args.Length > 1 && int.TryParse(args[1], out var parsedSeconds) ? parsedSeconds : 20;
    var viewportWidth = args.Length > 2 && int.TryParse(args[2], out var parsedWidth) ? parsedWidth : 1280;
    var viewportHeight = args.Length > 3 && int.TryParse(args[3], out var parsedHeight) ? parsedHeight : 720;
    var paintPageMode = args.Length > 4 ? args[4] : "animation";
    return CefUnity.Harness.PaintStatisticsCommand.Run(durationSeconds, viewportWidth, viewportHeight, paintPageMode);
}
if (command == "zero-frame-wait")
{
    // usage: zero-frame-wait [seconds] [zeroFrameWaitMilliseconds] [width] [height] [animation|intermittent]
    var durationSeconds = args.Length > 1 && int.TryParse(args[1], out var waitSeconds) ? waitSeconds : 20;
    var zeroFrameWaitMilliseconds = args.Length > 2 && float.TryParse(args[2], out var parsedWait) ? parsedWait : 10f;
    var width = args.Length > 3 && int.TryParse(args[3], out var parsedW) ? parsedW : 1920;
    var height = args.Length > 4 && int.TryParse(args[4], out var parsedH) ? parsedH : 1080;
    var pageMode = args.Length > 5 ? args[5] : "animation";
    return CefUnity.Harness.ZeroFrameWaitCommand.Run(durationSeconds, zeroFrameWaitMilliseconds, width, height, pageMode);
}
if (command == "lifecycle")
{
    // usage: lifecycle [cycles] [listenPort] [framesPerCycle]
    var cycles = args.Length > 1 && int.TryParse(args[1], out var parsedCycles) ? parsedCycles : 5;
    var listenPort = args.Length > 2 && int.TryParse(args[2], out var parsedPort) ? parsedPort : 11564;
    var framesPerCycle = args.Length > 3 && int.TryParse(args[3], out var parsedFrames) ? parsedFrames : 60;
    return CefUnity.Harness.LifecycleCommand.Run(cycles, listenPort, framesPerCycle);
}
if (command == "replay")
{
    if (args.Length < 2) { Console.Error.WriteLine("usage: replay <recording-csv>"); return 2; }
    var result = CefUnity.Runtime.ScrollReplayRunner.Run(File.ReadLines(args[1]));
    if (!result.Ok) { Console.Error.WriteLine($"REPLAY FAIL: {result.Error}"); return 1; }
    Console.WriteLine($"REPLAY ok events={result.Events} ticks={result.Ticks} mismatches={result.Mismatches}/{result.Ticks}");
    return 0;
}
Console.Error.WriteLine($"unknown command: {command}");
return 2;
