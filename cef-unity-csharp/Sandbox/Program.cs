using Interop;

CefRuntime.Init();

using (var browser = new Browser(1920*2, 1080*2, "https://www.google.com"))
{
    for (var i = 0; i < 60000; i++)
    {
        CefRuntime.Pump();
        Thread.Sleep(100);
        if (browser.TryGetBuffer(out var bgra, out var w, out var h))
        {
            Console.WriteLine($"Got frame: {w}x{h}, {bgra.Length} bytes");

            // BGRA -> RGB for PPM
            var rgb = new byte[w * h * 3];
            for (var p = 0; p < w * h; p++)
            {
                rgb[p * 3]     = bgra[p * 4 + 2]; // R
                rgb[p * 3 + 1] = bgra[p * 4 + 1]; // G
                rgb[p * 3 + 2] = bgra[p * 4];     // B
            }

            var outPath = Path.Combine(AppContext.BaseDirectory, "output.ppm");
            using var fs = File.Create(outPath);
            using var sw = new StreamWriter(fs, System.Text.Encoding.ASCII);
            sw.Write($"P6\n{w} {h}\n255\n");
            sw.Flush();
            fs.Write(rgb);

            Console.WriteLine($"Saved {outPath}");

            // マウスイベント動作確認: ページ中央をクリック
            var cx = w / 2;
            var cy = h / 2;
            Console.WriteLine($"Sending mouse click at ({cx}, {cy})");
            browser.SendMouseMove(cx, cy);
            browser.SendMouseClick(cx, cy, MouseButton.Left, false);  // mouse down
            browser.SendMouseClick(cx, cy, MouseButton.Left, true);   // mouse up
            Thread.Sleep(500);

            CefRuntime.Shutdown();
            return;
        }
    }

    Console.WriteLine("No new frame yet");
}

CefRuntime.Shutdown();
