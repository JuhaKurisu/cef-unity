using System.Diagnostics;
using Interop;

// User-Agent 確認 + SPA遅延がAIレスポンス時間なのか確認

CefRuntime.Init();
using var browser = new Browser(1280, 720, "https://www.google.com/search?q=test");

long WaitForFirstFrame(string label, int timeoutMs)
{
    var sw = Stopwatch.StartNew();
    long firstFrameMs = -1;
    var frames = 0;
    while (sw.ElapsedMilliseconds < timeoutMs)
    {
        CefRuntime.Pump();
        Thread.Sleep(16);
        if (browser.TryGetBuffer(out _, out _, out _))
        {
            frames++;
            if (firstFrameMs < 0) firstFrameMs = sw.ElapsedMilliseconds;
        }
    }
    Console.WriteLine($"  [{label}] first_frame={firstFrameMs}ms, frames={frames}");
    return firstFrameMs;
}

Console.WriteLine("=== 初期ロード ===");
WaitForFirstFrame("load", 5000);

// User-Agent をタイトルに設定
browser.ExecuteJavaScript("document.title = navigator.userAgent;");
Thread.Sleep(500);
for (int i = 0; i < 30; i++) { CefRuntime.Pump(); Thread.Sleep(16); }

// JS で fetch タイミングを計測: SPA ナビゲーション中の実際のネットワーク時間
Console.WriteLine("\n=== AI Mode fetch timing (JS performance.now) ===");
browser.ExecuteJavaScript(@"
    (function() {
        var start = performance.now();
        // AI Mode リンクをクリック
        var link = document.querySelector('a[href*=""udm=50""]');
        if (!link) { document.title = 'NO_AI_LINK'; return; }

        // PerformanceObserver で fetch/navigation 時間を追跡
        var observer = new PerformanceObserver(function(list) {
            list.getEntries().forEach(function(e) {
                if (e.name.includes('udm=50') || e.name.includes('aio')) {
                    console.log('PERF: ' + e.entryType + ' ' + e.name.substring(0,80) + ' duration=' + Math.round(e.duration) + 'ms responseStart=' + Math.round(e.responseStart) + 'ms');
                }
            });
        });
        observer.observe({entryTypes: ['resource', 'navigation']});

        // MutationObserver で DOM 変更を追跡
        var firstMutation = 0;
        var mutCount = 0;
        var mutObserver = new MutationObserver(function(mutations) {
            mutCount += mutations.length;
            if (!firstMutation) {
                firstMutation = performance.now() - start;
                console.log('FIRST_MUTATION: ' + Math.round(firstMutation) + 'ms after click, mutations=' + mutations.length);
            }
        });
        mutObserver.observe(document.body, {childList: true, subtree: true, attributes: true, characterData: true});

        document.title = 'UA:' + navigator.userAgent.substring(0, 100);
        link.click();
        console.log('CLICK_TIME: ' + Math.round(performance.now() - start) + 'ms');
    })();
");
WaitForFirstFrame("click_ai", 8000);

// サーバーログのon_paintタイミング
var logPath = Path.Combine(Path.GetTempPath(), "cef_unity_server.log");
if (File.Exists(logPath))
{
    var lines = File.ReadAllLines(logPath);
    var paintLines = lines.Where(l => l.Contains("on_paint")).ToArray();
    Console.WriteLine($"\non_paint total: {paintLines.Length}");
}

CefRuntime.Shutdown();
