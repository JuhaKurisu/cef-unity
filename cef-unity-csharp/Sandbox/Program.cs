using System.Diagnostics;
using Interop;

const string Query = "openai";
const string SearchUrl = $"https://www.google.com/search?q={Query}&hl=en";
const int Width = 1280;
const int Height = 720;

var tempDir = Path.GetTempPath();
var serverLogPath = Path.Combine(tempDir, "cef_unity_server.log");
var debugLogPath = Path.Combine(tempDir, "cef_unity_debug.log");

ResetLog(serverLogPath);
ResetLog(debugLogPath);

CefRuntime.Init();
using var browser = new Browser(Width, Height, SearchUrl);

Console.WriteLine($"Search URL: {SearchUrl}");

DrainFrames(500);
var initialLoad = WaitForFirstFrame("initial_load", 8000);
DrainFrames(1000);

var aiMode = RunScenario(
    "search_to_ai_mode",
    @"
(() => {
  const selectors = [
    'a[href*=""udm=50""]',
    'a[href*=""sca_esv=""][href*=""udm=50""]'
  ];
  let link = null;
  for (const selector of selectors) {
    link = document.querySelector(selector);
    if (link) break;
  }
  if (link) {
    link.click();
  }
})();
",
    url => url.Contains("udm=50", StringComparison.OrdinalIgnoreCase),
    8000);

DrainFrames(1500);

var images = RunScenario(
    "ai_mode_to_images",
    @"
(() => {
  const selectors = [
    'a[href*=""tbm=isch""]',
    'a[href*=""udm=2""]'
  ];
  let link = null;
  for (const selector of selectors) {
    link = document.querySelector(selector);
    if (link) break;
  }
  if (link) {
    link.click();
  }
})();
",
    url => url.Contains("tbm=isch", StringComparison.OrdinalIgnoreCase) || url.Contains("udm=2", StringComparison.OrdinalIgnoreCase),
    8000);

Console.WriteLine();
Console.WriteLine("Summary:");
Console.WriteLine($"  initial_load: {initialLoad.FirstFrameMs}ms");
Console.WriteLine($"  search_to_ai_mode: first_frame={aiMode.FirstFrameMs}ms url_changed={aiMode.UrlChangedMs}ms matched={aiMode.UrlMatched}");
Console.WriteLine($"  ai_mode_to_images: first_frame={images.FirstFrameMs}ms url_changed={images.UrlChangedMs}ms matched={images.UrlMatched}");

PrintInterestingLogLines(serverLogPath);
PrintInterestingLogLines(debugLogPath);

CefRuntime.Shutdown();

return;

ScenarioResult RunScenario(string name, string clickScript, Func<string, bool> urlMatched, int timeoutMs)
{
    Console.WriteLine();
    Console.WriteLine($"=== {name} ===");

    var logStart = GetLogLineCount(serverLogPath);
    var beforeUrl = browser.GetUrl();
    Console.WriteLine($"  before_url={beforeUrl}");
    browser.ExecuteJavaScript($$"""
        (() => {
          console.log('[TEST] {{name}} location_before=' + location.href);
          {{clickScript}}
        })();
        """);

    var result = WaitForNavigationAndFrame(name, beforeUrl, urlMatched, timeoutMs);
    DrainFrames(1200);
    PrintNewLogLines(serverLogPath, logStart, name);
    return result;
}

ScenarioResult WaitForNavigationAndFrame(string label, string beforeUrl, Func<string, bool> urlMatched, int timeoutMs)
{
    var sw = Stopwatch.StartNew();
    long firstFrameMs = -1;
    long urlChangedMs = -1;
    var frames = 0;
    var finalUrl = beforeUrl;
    var matched = false;

    while (sw.ElapsedMilliseconds < timeoutMs)
    {
        CefRuntime.Pump();
        if (browser.TryGetBuffer(out _, out _, out _))
        {
            frames++;
            if (firstFrameMs < 0)
                firstFrameMs = sw.ElapsedMilliseconds;
        }

        finalUrl = browser.GetUrl();
        matched = urlMatched(finalUrl);
        if (urlChangedMs < 0 && !string.Equals(finalUrl, beforeUrl, StringComparison.Ordinal))
            urlChangedMs = sw.ElapsedMilliseconds;

        if (matched && firstFrameMs >= 0)
            break;

        Thread.Sleep(16);
    }

    Console.WriteLine($"  [{label}] first_frame={firstFrameMs}ms url_changed={urlChangedMs}ms frames={frames}");
    Console.WriteLine($"  [{label}] after_url={finalUrl}");
    return new ScenarioResult(label, firstFrameMs, urlChangedMs, frames, matched, finalUrl);
}

ScenarioResult WaitForFirstFrame(string label, int timeoutMs)
{
    var sw = Stopwatch.StartNew();
    long firstFrameMs = -1;
    var frames = 0;
    var finalUrl = browser.GetUrl();

    while (sw.ElapsedMilliseconds < timeoutMs)
    {
        CefRuntime.Pump();
        if (browser.TryGetBuffer(out _, out _, out _))
        {
            frames++;
            if (firstFrameMs < 0)
                firstFrameMs = sw.ElapsedMilliseconds;
        }

        finalUrl = browser.GetUrl();
        if (firstFrameMs >= 0)
            break;

        Thread.Sleep(16);
    }

    Console.WriteLine($"  [{label}] first_frame={firstFrameMs}ms frames={frames}");
    return new ScenarioResult(label, firstFrameMs, -1, frames, true, finalUrl);
}

void DrainFrames(int durationMs)
{
    var sw = Stopwatch.StartNew();
    while (sw.ElapsedMilliseconds < durationMs)
    {
        CefRuntime.Pump();
        browser.TryGetBuffer(out _, out _, out _);
        Thread.Sleep(16);
    }
}

static void ResetLog(string path)
{
    try
    {
        File.WriteAllText(path, string.Empty);
    }
    catch
    {
    }
}

static int GetLogLineCount(string path)
{
    if (!File.Exists(path))
        return 0;

    return File.ReadLines(path).Count();
}

static void PrintNewLogLines(string path, int startLine, string label)
{
    if (!File.Exists(path))
        return;

    var newLines = File.ReadLines(path)
        .Skip(startLine)
        .Where(line =>
            line.Contains("[TEST]", StringComparison.Ordinal) ||
            line.Contains("[JS]", StringComparison.Ordinal) ||
            line.Contains("inject_google_osr_workarounds", StringComparison.Ordinal))
        .ToArray();

    if (newLines.Length == 0)
        return;

    Console.WriteLine($"  [{label}] log excerpts:");
    foreach (var line in newLines)
        Console.WriteLine($"    {line}");
}

static void PrintInterestingLogLines(string path)
{
    if (!File.Exists(path))
        return;

    var lines = File.ReadLines(path)
        .Where(line =>
            line.Contains("[TEST]", StringComparison.Ordinal) ||
            line.Contains("[JS]", StringComparison.Ordinal) ||
            line.Contains("inject_google_osr_workarounds", StringComparison.Ordinal))
        .ToArray();

    if (lines.Length == 0)
        return;

    Console.WriteLine();
    Console.WriteLine($"Interesting lines from {Path.GetFileName(path)}:");
    foreach (var line in lines)
        Console.WriteLine($"  {line}");
}

readonly record struct ScenarioResult(string Label, long FirstFrameMs, long UrlChangedMs, int Frames, bool UrlMatched, string FinalUrl);
