using Interop;

CefRuntime.Init();

using (var browser = new Browser(1920, 1080, "https://example.com"))
{
    // tick を数回回してブラウザを動かす
    for (var i = 0; i < 10000; i++)
    {
        CefRuntime.Tick();
        Thread.Sleep(16);
        
        if (browser.TryGetBuffer(out var buffer, out var w, out var h))
        {
            Console.WriteLine($"Got frame: {w}x{h}, {buffer.Length} bytes");
            CefRuntime.Shutdown();
            return;
        }
    }
    
    Console.WriteLine($"No new frame yet");
}

CefRuntime.Shutdown();
