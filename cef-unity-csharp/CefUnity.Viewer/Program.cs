using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using CefUnity.Interop;
using CefUnity.Runtime;
using CefUnity.Viewer;
using Silk.NET.SDL;

if (args.Length > 0 && args[0] == "spike")
{
    // spike パスでも SDL 初期化前設定を適用する (Item F: spike branch)
    // classic TEXTEDITING は 32 バイト固定で長い変換文字列が断片化するため、全文が届く TEXTEDITING_EXT を有効化する。
    Environment.SetEnvironmentVariable("SDL_IME_SUPPORT_EXTENDED_TEXT", "1");
    MacMomentumScrollSupport.Enable();
    return SpikeRunner.Run();
}

var viewerOptions = ViewerOptions.Parse(args);
if (viewerOptions == null)
{
    Console.Error.WriteLine(ViewerOptions.Usage);
    return 2;
}

if (viewerOptions.AnalyzePath != null)
{
    // --analyze は SDL/CEF を起動せず即座に結果を返す (Item F: --analyze early return)
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

// --replay ファイルの検証を CEF 初期化より前に行う (Item E)
// ファイル不在 / 空の場合は即座に exit 2 し、CEF を起動しない。
CefUnity.Runtime.ScrollReplaySource? replaySource = null;
if (viewerOptions.ReplayPath != null)
{
    IEnumerable<string> replayLines;
    try
    {
        replayLines = File.ReadLines(viewerOptions.ReplayPath).ToList();
    }
    catch (Exception exception) when (exception is IOException or FileNotFoundException)
    {
        Console.Error.WriteLine($"replay: {viewerOptions.ReplayPath} を読み込めません: {exception.Message}");
        return 2;
    }
    replaySource = new ScrollReplaySource(replayLines);
    if (replaySource.TotalEvents == 0)
    {
        Console.Error.WriteLine($"replay: {viewerOptions.ReplayPath} に over=1 の E 行がない");
        return 2;
    }
    Console.WriteLine($"replay: {replaySource.TotalEvents} events");
}

// --record 開始前に旧セッションの録画 CSV を削除して追記混入を防ぐ (Item B)
if (viewerOptions.Record)
{
    var recordingPath = Path.Combine(Path.GetTempPath(), "cef_scroll_events.csv");
    if (File.Exists(recordingPath)) File.Delete(recordingPath);
}

// Item F: --analyze / usage-error パスを抜けた後、CEF/窓 起動前に SDL 初期化前設定を適用する
// classic TEXTEDITING は 32 バイト固定で長い変換文字列が断片化するため、全文が届く TEXTEDITING_EXT を有効化する。
Environment.SetEnvironmentVariable("SDL_IME_SUPPORT_EXTENDED_TEXT", "1");
MacMomentumScrollSupport.Enable();

CefRuntime.Initialize(useGpu: true);
D3D11GraphicsDevice? graphicsDevice = null;
try
{
    var rendererKind = FrameRendererFactory.SelectKind();
    if (rendererKind == FrameRendererKind.Unsupported)
    {
        Console.Error.WriteLine("このプラットフォームには表示バックエンドがありません (macOS / Windows / Linux のみ対応)");
        return 1;
    }
    if (rendererKind == FrameRendererKind.Direct3D11)
    {
        // 共有 fence は cef_unity_create_browser の中で開かれるため、デバイス注入は
        // 必ず CefFrameSource (= Browser) 生成より前に行う。後から注入すると GPU 同期が張られない。
        graphicsDevice = new D3D11GraphicsDevice();
        Browser.SetExternalD3D11Device(graphicsDevice.DevicePointer);
        if (!Browser.IsD3D11Connected())
        {
            Console.Error.WriteLine("D3D11 デバイスの注入に失敗しました (native 側が接続を認識していません)");
            foreach (var line in CefRuntime.GetLogs()) Console.Error.WriteLine($"[cef] {line}");
            return 1;
        }
    }

    using var frameSource = new CefFrameSource(viewerOptions.Width, viewerOptions.Height, viewerOptions.Url);
    using var scrollMatrix = new ScrollInputMatrix();
    if (replaySource != null)
        scrollMatrix.AttachSource(replaySource);
    using var statistics = viewerOptions.StatisticsPath != null
        ? new StatisticsRecorder(viewerOptions.StatisticsPath) : null;
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource, scrollMatrix,
        RendererFactoryFor(rendererKind, graphicsDevice), statistics, replaySource);
    viewerWindow.Run();
}
catch (Exception exception)
{
    Console.Error.WriteLine($"FATAL: {exception}");
    foreach (var line in CefRuntime.GetLogs()) Console.Error.WriteLine($"[cef] {line}");
    Console.Error.WriteLine(RecoveryHint());
    return 1;
}
finally
{
    // native 側はデバイスを AddRef せず借用するだけなので、CEF shutdown 後に解放する
    CefRuntime.Shutdown();
    graphicsDevice?.Dispose();
}
return 0;

static Func<Sdl, IFrameRenderer> RendererFactoryFor(FrameRendererKind kind, D3D11GraphicsDevice? graphicsDevice)
{
    // Metal は ViewerWindow が持つ Sdl インスタンスを要求するため生成をウィンドウ側まで遅延させる。
    // D3D11 は Sdl を使わない (HWND は IView から取る)。
    if (kind == FrameRendererKind.Direct3D11)
        return _ => new D3D11FrameRenderer(graphicsDevice!);
    // Linux も Sdl を使わない (GL コンテキストは IView から取る)。
    if (kind == FrameRendererKind.OpenGL)
        return _ => new OpenGLFrameRenderer();
    return sdl => new MetalFrameRenderer(sdl);
}

static string RecoveryHint()
    => RuntimeInformation.IsOSPlatform(OSPlatform.Windows)
        ? "復旧手順: サーバー残留は `taskkill /IM cef-unity-server.exe /F`、起動ハングはキャッシュ破損の可能性 → %TEMP% の cef_unity_cache を削除"
        : "復旧手順: サーバー残留は `pkill -f cef-unity-server`、起動ハングはキャッシュ破損の可能性 → $TMPDIR の cef_unity_cache を削除";
