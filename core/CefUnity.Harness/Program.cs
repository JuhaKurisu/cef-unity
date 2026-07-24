using CefUnity.Interop;

// サブコマンド: (なし)=スモーク, replay=Phase 4 で追加
var cmd = args.Length > 0 ? args[0] : "smoke";
if (cmd == "smoke")
{
    var frames = 0;
    CefRuntime.Init(useGpu: false);
    using (var browser = new Browser(1280, 720, "https://example.com"))
    {
        for (var i = 0; i < 600; i++)
        {
            browser.SendExternalBeginFrame((ulong)i);
            CefRuntime.Pump();
            Thread.Sleep(16);
            if (browser.TryGetBuffer(out var bgra, out var w, out var h))
            {
                frames++;
                if (frames <= 3 || frames % 25 == 0)
                    Console.WriteLine($"Frame #{frames}: {w}x{h}, {bgra.Length} bytes");
            }
        }
        Console.WriteLine($"SMOKE_OK frames={frames}");
    }
    CefRuntime.Shutdown();
    return frames > 0 ? 0 : 1;
}
if (cmd == "replay")
{
    if (args.Length < 2) { Console.Error.WriteLine("usage: replay <recording-csv>"); return 2; }
    var result = CefUnity.Runtime.ScrollReplayRunner.Run(File.ReadLines(args[1]));
    if (!result.Ok) { Console.Error.WriteLine($"REPLAY FAIL: {result.Error}"); return 1; }
    Console.WriteLine($"REPLAY ok events={result.Events} ticks={result.Ticks} mismatches={result.Mismatches}/{result.Ticks}");
    return 0;
}
Console.Error.WriteLine($"unknown command: {cmd}");
return 2;
