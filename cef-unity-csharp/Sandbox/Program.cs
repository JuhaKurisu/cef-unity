using Interop;

CefRuntime.Init();

using (var browser = new Browser(1280, 720, "https://youtu.be/dQw4w9WgXcQ"))
{
    var frameCount = 0;
    var snapshotAt = new HashSet<int> { 1, 10, 50, 100, 200, 300, 400, 500 };
    for (var i = 0; i < 600; i++) // 60秒間キャプチャ
    {
        CefRuntime.Pump();
        Thread.Sleep(100);
        if (browser.TryGetBuffer(out var bgra, out var w, out var h))
        {
            frameCount++;
            if (frameCount <= 5 || frameCount % 50 == 0)
                Console.WriteLine($"Frame #{frameCount}: {w}x{h}, {bgra.Length} bytes (t={i*100}ms)");

            if (snapshotAt.Contains(frameCount))
            {
                var rgb = new byte[w * h * 3];
                for (var p = 0; p < w * h; p++)
                {
                    rgb[p * 3]     = bgra[p * 4 + 2];
                    rgb[p * 3 + 1] = bgra[p * 4 + 1];
                    rgb[p * 3 + 2] = bgra[p * 4];
                }
                var outPath = Path.Combine(AppContext.BaseDirectory, $"youtube_{frameCount}.ppm");
                using var fs = File.Create(outPath);
                using var sw = new StreamWriter(fs, System.Text.Encoding.ASCII);
                sw.Write($"P6\n{w} {h}\n255\n");
                sw.Flush();
                fs.Write(rgb);
                Console.WriteLine($"  -> Saved {outPath}");
            }
        }
    }

    Console.WriteLine($"Total frames: {frameCount}");
}

CefRuntime.Shutdown();
