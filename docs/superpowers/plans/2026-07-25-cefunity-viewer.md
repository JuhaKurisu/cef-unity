# CefUnity.Viewer (Silk.NET 単体ブラウザ) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unity 外で CEF を表示+操作できる mac 単体ブラウザを作り、スクロールカクツキの Unity 固有性を切り分ける。

**Architecture:** `core/CefUnity.Viewer` (net10.0 exe) が CefUnity.Core だけを参照。Silk.NET の SDL バックエンド窓 + CAMetalLayer への C# blit で GPU ゼロコピー表示 (useGpu:true / IOSurface)。スクロールは Core の ScrollInputPipeline をそのまま使い 3 モード実行時切替 + 録画/リプレイ。Rust 変更ゼロ。

**Tech Stack:** .NET 10 / Silk.NET 2.22.0 (Windowing+Sdl, Input+Sdl, SDL) / objc_msgSend P/Invoke (Metal) / NUnit (既存 CefUnity.Tests)

**Spec:** `docs/superpowers/specs/2026-07-25-silknet-viewer-design.md`

## Global Constraints

- 識別子は省略形禁止・常にフルネーム (ルート CLAUDE.md の命名規約。維持可: id, url, ime, gpu, fps, ipc, ffi, osr, cef, bgra, io, x, y, tau, app, config, info, max, min, delta, sync, log)
- 対象 OS は **macOS のみ** (Windows は保留 — spec 参照)
- **Rust / dylib / csbindgen は一切変更しない**
- namespace: Viewer 新規コードは `CefUnity.Viewer`、Core 追加コードは `CefUnity.Runtime`
- Core (netstandard2.1) に新しい PackageReference を追加しない (Silk.NET 依存コードは Viewer 側に置く)
- コミットはプレーンな `git commit` のみ (`--author` / Co-Authored-By 禁止)
- テストは既存 `core/CefUnity.Tests` (NUnit) に追加し、`dotnet test core/CefUnity.Tests` で Unity なしで走ること
- 手動確認コマンドは全て `core/` からの相対で記載。Viewer 実行は `dotnet run --project core/CefUnity.Viewer -- <args>`
- ⚠️ CEF サーバープロセスはシングルトン。異常終了で残留したら `pkill -f cef-unity-server`。並行実行しない

## 主要な既存 API (全タスク共通の前提)

```csharp
// CefUnity.Interop (core/CefUnity.Core/Interop/CefUnity.cs)
CefRuntime.Initialize(bool useGpu = true, bool enableLog = false); CefRuntime.Pump(); CefRuntime.Shutdown(); CefRuntime.GetLogs();
new Browser(int width, int height, string url) : IDisposable
browser.SendExternalBeginFrame(ulong unityFrame); browser.Resize(int width, int height); browser.LoadUrl(string url);
browser.SendMouseMove(int x, int y, uint modifiers = 0);
browser.SendMouseClick(int x, int y, MouseButton button, bool mouseUp, int clickCount = 1, uint modifiers = 0);
browser.SendMouseWheel(int x, int y, int deltaX, int deltaY, uint modifiers = 0);
browser.SendKeyEvent(KeyEventType eventType, CefKeyCode key, uint modifiers = 0);
browser.SendCharEvent(char character, uint modifiers = 0);
browser.ImeSetComposition(string text, uint selectionStart, uint selectionEnd);
browser.ImeCommitText(string text); browser.ImeFinishComposingText(bool keepSelection = false); browser.ImeCancelComposition();
browser.GetImeCaret(out int x, out int y, out int width, out int height);
browser.PeekAcceleratedFrameId(); // ulong 単調増加 paint カウンタ (消費しない)
Browser.TryReceiveIOSurfaceTexture(out IntPtr texturePointer, out int width, out int height, out uint format); // static
Browser.ReleaseMetalTexture(IntPtr texture); // static
Browser.IsIOSurfaceConnected(); // static
// enum CefEventFlags: ShiftDown=1<<1, ControlDown=1<<2, AltDown=1<<3, CommandDown=1<<7 (CefUnity.cs:48)
// CefKeyCodes: Backspace/Tab/Return/Escape/Delete/矢印/Home/End/PageUp/PageDown/F1-F12/Keypad*/修飾キー 定義済み

// CefUnity.Runtime (core/CefUnity.Core/ScrollInput/, Scroll/, Replay/)
var pipeline = new ScrollInputPipeline(); // WheelPixelsPerStep=60f, SmoothTau=0.015f
pipeline.StartNativeSource(out Exception error); // NativeScrollSourceStart.Started/NotSupported/Unavailable/Failed
pipeline.AttachSource(IScrollEventSource source); pipeline.HasNativeSource; pipeline.Predictive;
pipeline.AddWheelSteps(float xSteps, float ySteps, float resolutionScale);
pipeline.Drain(bool overBrowser, float resolutionScale); // 毎フレーム。順序: Drain → TickResampler → 送信 → TickSmoother → 送信
pipeline.TickResampler(out int deltaX, out int deltaY); pipeline.TickSmoother(float deltaTime, out int deltaX, out int deltaY);
pipeline.Reset(); pipeline.RecordingEnabled; pipeline.FlushRecording(); // 録画は $TMPDIR/cef_scroll_events.csv
interface IScrollEventSource : IDisposable { bool Start(); int Poll(ScrollInputEvent[] buffer); double Now { get; } }
struct ScrollInputEvent { double Timestamp; float DeltaXPixels; float DeltaYPixels; bool Precise; ScrollPhase Phase; }
// 録画 CSV 形式: S,scale / E,ts,dx,dy,phase,precise,over / T,now,dx,dy,predictive
ScrollReplayRunner.Run(IEnumerable<string> csvLines); // オフライン忠実度照合 (ライブ再生ではない)
```

---

### Task 1: プロジェクト骨格 + ViewerOptions (CLI 解析)

**Files:**
- Create: `core/CefUnity.Viewer/CefUnity.Viewer.csproj`
- Create: `core/CefUnity.Viewer/Directory.Build.targets`
- Create: `core/CefUnity.Viewer/ViewerOptions.cs`
- Create: `core/CefUnity.Viewer/Program.cs` (仮実装)
- Modify: `core/CefUnity.slnx` (プロジェクト追加)
- Modify: `core/CefUnity.Tests/CefUnity.Tests.csproj` (Viewer 参照追加)
- Test: `core/CefUnity.Tests/ViewerOptionsTests.cs`

**Interfaces:**
- Produces: `CefUnity.Viewer.ViewerOptions` — `static ViewerOptions? Parse(string[] arguments)` (null = 解析失敗)、プロパティ `string Url` / `int Width` / `int Height` / `ScrollMode Mode` / `bool Record` / `string? ReplayPath` / `string? StatisticsPath` / `string? AnalyzePath`、`static string Usage`
- Produces: `CefUnity.Viewer.ScrollMode` enum — `Raw / Smoother / Resampler`

- [ ] **Step 1: csproj と Directory.Build.targets を作成**

`core/CefUnity.Viewer/CefUnity.Viewer.csproj` (Harness と同型。CopyServerApp はバンドル既存時スキップ — テストビルドを遅くしないため):

```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <RustProjectDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust'))</RustProjectDir>
    <RustTargetDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust\target\debug'))</RustTargetDir>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\CefUnity.Core\CefUnity.Core.csproj" />
    <PackageReference Include="Silk.NET.Windowing" Version="2.22.0" />
    <PackageReference Include="Silk.NET.Windowing.Sdl" Version="2.22.0" />
    <PackageReference Include="Silk.NET.Input" Version="2.22.0" />
    <PackageReference Include="Silk.NET.Input.Sdl" Version="2.22.0" />
    <PackageReference Include="Silk.NET.SDL" Version="2.22.0" />
  </ItemGroup>
  <ItemGroup>
    <None Include="$(RustTargetDir)/libcef_unity_rust.dylib"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.dylib"
          Condition="$([MSBuild]::IsOSPlatform('OSX'))" />
  </ItemGroup>
  <!-- cef-unity-server.app を出力先に用意 (Harness と同じ)。既存ならスキップ (dotnet test を遅くしない) -->
  <Target Name="CopyServerApp" AfterTargets="Build"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And !Exists('$(OutputPath)cef-unity-server.app')">
    <Exec Command="bash '$(RustProjectDir)/build-server-sandbox.sh' '$(OutputPath)'" />
  </Target>
</Project>
```

`core/CefUnity.Viewer/Directory.Build.targets` は `core/CefUnity.Harness/Directory.Build.targets` の内容をそのままコピーする (CEF framework の rsync、`!Exists` 条件付き)。

- [ ] **Step 2: 失敗するテストを書く**

`core/CefUnity.Tests/ViewerOptionsTests.cs`:

```csharp
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ViewerOptionsTests
    {
        [Test]
        public void Parse_NoArguments_ReturnsDefaults()
        {
            var options = ViewerOptions.Parse(new string[0]);
            Assert.That(options, Is.Not.Null);
            Assert.That(options!.Url, Is.EqualTo("https://example.com"));
            Assert.That(options.Width, Is.EqualTo(1280));
            Assert.That(options.Height, Is.EqualTo(720));
            Assert.That(options.Mode, Is.EqualTo(ScrollMode.Resampler));
            Assert.That(options.Record, Is.False);
            Assert.That(options.ReplayPath, Is.Null);
            Assert.That(options.StatisticsPath, Is.Null);
            Assert.That(options.AnalyzePath, Is.Null);
        }

        [Test]
        public void Parse_AllArguments_ParsesEveryField()
        {
            var options = ViewerOptions.Parse(new[]
            {
                "--url", "https://ja.wikipedia.org", "--size", "1920x1080",
                "--scroll-mode", "smoother", "--record",
                "--replay", "/tmp/replay.csv", "--statistics", "/tmp/statistics.csv",
            });
            Assert.That(options, Is.Not.Null);
            Assert.That(options!.Url, Is.EqualTo("https://ja.wikipedia.org"));
            Assert.That(options.Width, Is.EqualTo(1920));
            Assert.That(options.Height, Is.EqualTo(1080));
            Assert.That(options.Mode, Is.EqualTo(ScrollMode.Smoother));
            Assert.That(options.Record, Is.True);
            Assert.That(options.ReplayPath, Is.EqualTo("/tmp/replay.csv"));
            Assert.That(options.StatisticsPath, Is.EqualTo("/tmp/statistics.csv"));
        }

        [TestCase("raw", ScrollMode.Raw)]
        [TestCase("smoother", ScrollMode.Smoother)]
        [TestCase("resampler", ScrollMode.Resampler)]
        public void Parse_ScrollMode_MapsName(string name, ScrollMode expected)
        {
            var options = ViewerOptions.Parse(new[] { "--scroll-mode", name });
            Assert.That(options!.Mode, Is.EqualTo(expected));
        }

        [TestCase("--size", "abc")]
        [TestCase("--size", "100")]
        [TestCase("--scroll-mode", "unknown")]
        public void Parse_InvalidValue_ReturnsNull(string flag, string value)
        {
            Assert.That(ViewerOptions.Parse(new[] { flag, value }), Is.Null);
        }

        [Test]
        public void Parse_UnknownFlag_ReturnsNull()
        {
            Assert.That(ViewerOptions.Parse(new[] { "--frobnicate" }), Is.Null);
        }

        [Test]
        public void Parse_Analyze_ParsesPath()
        {
            var options = ViewerOptions.Parse(new[] { "--analyze", "/tmp/statistics.csv" });
            Assert.That(options!.AnalyzePath, Is.EqualTo("/tmp/statistics.csv"));
        }
    }
}
```

`core/CefUnity.Tests/CefUnity.Tests.csproj` の ProjectReference ItemGroup に追加:

```xml
    <ProjectReference Include="..\CefUnity.Viewer\CefUnity.Viewer.csproj" />
```

- [ ] **Step 3: テストが失敗する (コンパイルエラーになる) ことを確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet test core/CefUnity.Tests --filter ViewerOptionsTests 2>&1 | tail -5`
Expected: FAIL (`ViewerOptions` が存在しないコンパイルエラー)

- [ ] **Step 4: ViewerOptions を実装**

`core/CefUnity.Viewer/ViewerOptions.cs`:

```csharp
namespace CefUnity.Viewer
{
    public enum ScrollMode
    {
        Raw,
        Smoother,
        Resampler,
    }

    /// <summary>CLI 引数。解析失敗は null (呼び出し側が Usage を表示)。</summary>
    public sealed class ViewerOptions
    {
        public const string Usage =
            "usage: CefUnity.Viewer [--url <url>] [--size <width>x<height>]\n" +
            "                       [--scroll-mode raw|smoother|resampler] [--record]\n" +
            "                       [--replay <events-csv>] [--statistics <output-csv>]\n" +
            "                       [--analyze <statistics-csv>]";

        public string Url { get; private set; } = "https://example.com";
        public int Width { get; private set; } = 1280;
        public int Height { get; private set; } = 720;
        public ScrollMode Mode { get; private set; } = ScrollMode.Resampler;
        public bool Record { get; private set; }
        public string? ReplayPath { get; private set; }
        public string? StatisticsPath { get; private set; }
        public string? AnalyzePath { get; private set; }

        public static ViewerOptions? Parse(string[] arguments)
        {
            var options = new ViewerOptions();
            for (var index = 0; index < arguments.Length; index++)
            {
                string? Next() => index + 1 < arguments.Length ? arguments[++index] : null;
                switch (arguments[index])
                {
                    case "--url":
                        if (Next() is not { } url) return null;
                        options.Url = url;
                        break;
                    case "--size":
                        var size = Next()?.Split('x');
                        if (size is not { Length: 2 }
                            || !int.TryParse(size[0], out var width)
                            || !int.TryParse(size[1], out var height)) return null;
                        options.Width = width;
                        options.Height = height;
                        break;
                    case "--scroll-mode":
                        switch (Next())
                        {
                            case "raw": options.Mode = ScrollMode.Raw; break;
                            case "smoother": options.Mode = ScrollMode.Smoother; break;
                            case "resampler": options.Mode = ScrollMode.Resampler; break;
                            default: return null;
                        }
                        break;
                    case "--record":
                        options.Record = true;
                        break;
                    case "--replay":
                        if (Next() is not { } replayPath) return null;
                        options.ReplayPath = replayPath;
                        break;
                    case "--statistics":
                        if (Next() is not { } statisticsPath) return null;
                        options.StatisticsPath = statisticsPath;
                        break;
                    case "--analyze":
                        if (Next() is not { } analyzePath) return null;
                        options.AnalyzePath = analyzePath;
                        break;
                    default:
                        return null;
                }
            }
            return options;
        }
    }
}
```

`core/CefUnity.Viewer/Program.cs` (仮 — Task 3 で置き換える):

```csharp
using CefUnity.Viewer;

var viewerOptions = ViewerOptions.Parse(args);
if (viewerOptions == null)
{
    Console.Error.WriteLine(ViewerOptions.Usage);
    return 2;
}
Console.WriteLine($"CefUnity.Viewer options ok: {viewerOptions.Url} {viewerOptions.Width}x{viewerOptions.Height} mode={viewerOptions.Mode}");
return 0;
```

- [ ] **Step 5: slnx にプロジェクト追加してテストが通ることを確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity/core && dotnet sln CefUnity.slnx add CefUnity.Viewer/CefUnity.Viewer.csproj && cd .. && dotnet test core/CefUnity.Tests --filter ViewerOptionsTests 2>&1 | tail -3`
Expected: PASS (全 ViewerOptionsTests 緑)。既存テストも壊れていないことを `dotnet test core/CefUnity.Tests 2>&1 | tail -3` で確認 (全緑)

- [ ] **Step 6: Commit**

```bash
git add core/CefUnity.Viewer core/CefUnity.slnx core/CefUnity.Tests
git commit -m "feat: CefUnity.Viewer プロジェクト骨格と CLI 解析"
```

---

### Task 2: スパイク — SDL 窓 + Metal present + NSEvent monitor + IME イベント疎通

spec のスパイク S1/S2/S3 を CEF なしで一括検証する。**このタスクが失敗したら実装を止めて設計に戻る。**

**Files:**
- Create: `core/CefUnity.Viewer/MetalNative.cs`
- Create: `core/CefUnity.Viewer/IFrameRenderer.cs`
- Create: `core/CefUnity.Viewer/MetalFrameRenderer.cs`
- Create: `core/CefUnity.Viewer/SpikeRunner.cs`
- Modify: `core/CefUnity.Viewer/Program.cs` (spike サブコマンド分岐追加)

**Interfaces:**
- Produces: `IFrameRenderer` — `void Initialize(Silk.NET.Windowing.IView view)` / `void Present(IntPtr texturePointer, int width, int height)` / `IDisposable`。`Present(IntPtr.Zero, w, h)` はテクスチャなし present (スパイク用、drawable を回すだけ)
- Produces: `MetalNative` — objc P/Invoke 補助 (下記コードの internal static メソッド群)

- [ ] **Step 1: MetalNative を実装**

`core/CefUnity.Viewer/MetalNative.cs`:

```csharp
using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>objc_msgSend ベースの最小 Metal/CoreAnimation 呼び出し (MetalFrameRenderer 専用)。</summary>
    internal static class MetalNative
    {
        private const string LibraryObjC = "/usr/lib/libobjc.A.dylib";
        private const string LibraryMetal = "/System/Library/Frameworks/Metal.framework/Metal";

        [StructLayout(LayoutKind.Sequential)]
        internal struct CGSize
        {
            public double Width;
            public double Height;
        }

        [DllImport(LibraryObjC, EntryPoint = "sel_registerName")]
        internal static extern IntPtr Selector([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector, IntPtr argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector, IntPtr argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector, IntPtr argument1, IntPtr argument2);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidBoolMessage(IntPtr receiver, IntPtr selector, [MarshalAs(UnmanagedType.I1)] bool argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidCGSizeMessage(IntPtr receiver, IntPtr selector, CGSize argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPush")]
        internal static extern IntPtr AutoreleasePoolPush();

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPop")]
        internal static extern void AutoreleasePoolPop(IntPtr pool);

        [DllImport(LibraryMetal)]
        internal static extern IntPtr MTLCreateSystemDefaultDevice();
    }
}
```

- [ ] **Step 2: IFrameRenderer と MetalFrameRenderer を実装**

`core/CefUnity.Viewer/IFrameRenderer.cs`:

```csharp
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>表示抽象。将来の Windows D3D11FrameRenderer の継ぎ目 (spec 参照)。</summary>
    internal interface IFrameRenderer : IDisposable
    {
        void Initialize(IView view);

        /// <summary>
        ///     受信テクスチャをウィンドウへ表示する。texturePointer が IntPtr.Zero の場合は
        ///     blit せず drawable を回すだけ (起動直後・スパイク用)。
        ///     drawableSize はテクスチャサイズに追従し、リサイズ中は旧サイズの絵が
        ///     レイヤー境界にスケール表示される (spec のリサイズ節)。spec のインターフェース案に
        ///     あった Resize はこのサイズ追従に統合した (意図的な簡約)。
        /// </summary>
        void Present(IntPtr texturePointer, int width, int height);
    }
}
```

`core/CefUnity.Viewer/MetalFrameRenderer.cs`:

```csharp
using Silk.NET.SDL;
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     受信 MTLTexture を CAMetalLayer の drawable へ blit して表示する。
    ///     テクスチャは cef_unity_receive_iosurface_texture がシステム既定 Metal デバイスで
    ///     作るため、レイヤーにも MTLCreateSystemDefaultDevice を設定すれば同一デバイスで
    ///     blit できる (Apple Silicon は単一 GPU)。
    ///     色: CEF の BGRA バイトは sRGB エンコード済み。BGRA8Unorm どうしの blit は
    ///     変換なしでバイトがそのまま表示され、ウィンドウ既定色空間 (sRGB) で正しく見える。
    /// </summary>
    internal sealed unsafe class MetalFrameRenderer : IFrameRenderer
    {
        private static readonly IntPtr SelectorSetDevice = MetalNative.Selector("setDevice:");
        private static readonly IntPtr SelectorSetFramebufferOnly = MetalNative.Selector("setFramebufferOnly:");
        private static readonly IntPtr SelectorSetDrawableSize = MetalNative.Selector("setDrawableSize:");
        private static readonly IntPtr SelectorNewCommandQueue = MetalNative.Selector("newCommandQueue");
        private static readonly IntPtr SelectorNextDrawable = MetalNative.Selector("nextDrawable");
        private static readonly IntPtr SelectorTexture = MetalNative.Selector("texture");
        private static readonly IntPtr SelectorCommandBuffer = MetalNative.Selector("commandBuffer");
        private static readonly IntPtr SelectorBlitCommandEncoder = MetalNative.Selector("blitCommandEncoder");
        private static readonly IntPtr SelectorCopyFromTexture = MetalNative.Selector("copyFromTexture:toTexture:");
        private static readonly IntPtr SelectorEndEncoding = MetalNative.Selector("endEncoding");
        private static readonly IntPtr SelectorPresentDrawable = MetalNative.Selector("presentDrawable:");
        private static readonly IntPtr SelectorCommit = MetalNative.Selector("commit");
        private static readonly IntPtr SelectorRelease = MetalNative.Selector("release");

        private readonly Sdl _sdl;
        private void* _metalView;
        private IntPtr _layer;
        private IntPtr _commandQueue;
        private int _drawableWidth;
        private int _drawableHeight;

        public MetalFrameRenderer(Sdl sdl)
        {
            _sdl = sdl;
        }

        public void Initialize(IView view)
        {
            var window = (Window*)view.Native!.Sdl!.Value;
            _metalView = _sdl.MetalCreateView(window);
            if (_metalView == null) throw new InvalidOperationException("SDL_Metal_CreateView failed");
            _layer = (IntPtr)_sdl.MetalGetLayer(_metalView);
            if (_layer == IntPtr.Zero) throw new InvalidOperationException("SDL_Metal_GetLayer failed");
            var device = MetalNative.MTLCreateSystemDefaultDevice();
            MetalNative.VoidMessage(_layer, SelectorSetDevice, device);
            // blit の書き込み先にするため framebufferOnly を外す
            MetalNative.VoidBoolMessage(_layer, SelectorSetFramebufferOnly, false);
            _commandQueue = MetalNative.IntPtrMessage(device, SelectorNewCommandQueue);
            if (_commandQueue == IntPtr.Zero) throw new InvalidOperationException("newCommandQueue failed");
        }

        public void Present(IntPtr texturePointer, int width, int height)
        {
            if (width > 0 && height > 0 && (width != _drawableWidth || height != _drawableHeight))
            {
                MetalNative.VoidCGSizeMessage(_layer, SelectorSetDrawableSize,
                    new MetalNative.CGSize { Width = width, Height = height });
                _drawableWidth = width;
                _drawableHeight = height;
            }
            // Rust スレッドと同じ罠: pool なしだと Metal オブジェクトが蓄積しフレームスパイクになる
            var pool = MetalNative.AutoreleasePoolPush();
            try
            {
                var drawable = MetalNative.IntPtrMessage(_layer, SelectorNextDrawable);
                if (drawable == IntPtr.Zero) return;
                var commandBuffer = MetalNative.IntPtrMessage(_commandQueue, SelectorCommandBuffer);
                if (texturePointer != IntPtr.Zero)
                {
                    var drawableTexture = MetalNative.IntPtrMessage(drawable, SelectorTexture);
                    var blitEncoder = MetalNative.IntPtrMessage(commandBuffer, SelectorBlitCommandEncoder);
                    MetalNative.VoidMessage(blitEncoder, SelectorCopyFromTexture, texturePointer, drawableTexture);
                    MetalNative.VoidMessage(blitEncoder, SelectorEndEncoding);
                }
                MetalNative.VoidMessage(commandBuffer, SelectorPresentDrawable, drawable);
                MetalNative.VoidMessage(commandBuffer, SelectorCommit);
            }
            finally
            {
                MetalNative.AutoreleasePoolPop(pool);
            }
        }

        public void Dispose()
        {
            if (_commandQueue != IntPtr.Zero)
            {
                MetalNative.VoidMessage(_commandQueue, SelectorRelease);
                _commandQueue = IntPtr.Zero;
            }
            if (_metalView != null)
            {
                _sdl.MetalDestroyView(_metalView);
                _metalView = null;
            }
        }
    }
}
```

- [ ] **Step 3: SpikeRunner を実装**

`core/CefUnity.Viewer/SpikeRunner.cs` — SDL 窓を開き 300 フレーム回して S1/S2/S3 を検証する:

```csharp
using CefUnity.Runtime;
using Silk.NET.Maths;
using Silk.NET.SDL;
using Silk.NET.Windowing;
using Silk.NET.Windowing.Sdl;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     spec スパイク S1/S2/S3 の一括検証 (CEF なし)。
    ///     S1: MacNativeScrollSource (NSEvent monitor) が SDL イベントループ下で発火するか
    ///     S2: SDL API 取得 + AddEventWatch で TEXTEDITING/TEXTINPUT が見えるか
    ///     S3: SDL_Metal_CreateView → CAMetalLayer → present が動くか
    /// </summary>
    internal static class SpikeRunner
    {
        private static int _textEvents;

        public static unsafe int Run()
        {
            Window.PrioritizeSdl();
            var windowOptions = WindowOptions.Default with
            {
                API = GraphicsAPI.None,
                Size = new Vector2D<int>(640, 480),
                Title = "CefUnity.Viewer spike",
                FramesPerSecond = 0,
                UpdatesPerSecond = 0,
                VSync = false,
            };
            using var window = Window.Create(windowOptions);
            var sdl = SdlWindowing.GetExistingApi(window)
                      ?? throw new InvalidOperationException("SDL backend not active (S2 FAIL)");

            var renderer = new MetalFrameRenderer(sdl);
            var scrollSource = new MacNativeScrollSource();
            var scrollEvents = new ScrollInputEvent[256];
            var scrollCount = 0;
            var frames = 0;

            window.Load += () =>
            {
                renderer.Initialize(window);
                sdl.StartTextInput();
                sdl.AddEventWatch(new PfnEventFilter(EventWatch), null);
                // NSApp は SDL が作成済みのこの時点で開始する (scroll_monitor.m の前提)
                Console.WriteLine($"S1 scroll monitor started: {scrollSource.Start()}");
            };
            window.Render += _ =>
            {
                scrollCount += scrollSource.Poll(scrollEvents);
                renderer.Present(IntPtr.Zero, 640, 480);
                if (++frames >= 300) window.Close();
            };
            window.Run();
            scrollSource.Dispose();
            renderer.Dispose();

            Console.WriteLine($"SPIKE frames={frames} scrollEvents={scrollCount} textEvents={_textEvents}");
            Console.WriteLine("S3 OK (present 300 frames, no exception)");
            Console.WriteLine(scrollCount > 0 ? "S1 OK (scroll events observed)" : "S1 NG? — 窓上でトラックパッドスクロールしたか確認");
            Console.WriteLine(_textEvents > 0 ? "S2 OK (text events observed)" : "S2 NG? — 日本語入力でタイプしたか確認");
            return 0;
        }

        private static unsafe int EventWatch(void* userData, Event* sdlEvent)
        {
            var type = (EventType)sdlEvent->Type;
            if (type == EventType.Textediting || type == EventType.Textinput) _textEvents++;
            return 1;
        }
    }
}
```

`core/CefUnity.Viewer/Program.cs` の先頭に分岐を追加 (ViewerOptions 解析の前):

```csharp
if (args.Length > 0 && args[0] == "spike") return CefUnity.Viewer.SpikeRunner.Run();
```

- [ ] **Step 4: スパイクを実行して S1/S2/S3 を目視確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet run --project core/CefUnity.Viewer -- spike`
手順: 窓が出ている 5 秒間に (1) 窓上でトラックパッドを 2 本指スクロール (2) 日本語 IME に切り替えて適当にタイプ。
Expected: `S3 OK` と `S1 OK` と `S2 OK` が出る。
**S1 NG の場合**: NSEvent monitor が SDL ループで動かない → 設計に戻る (代替: SDL MOUSEWHEEL の preciseX/preciseY + timestamp を IScrollEventSource 化)。
**S2 NG の場合**: Silk.NET SDL バックエンドの API 取得不可 → 設計に戻る (代替: Silk.NET.SDL 直接使用で窓を自前管理)。
**S3 NG (例外/クラッシュ) の場合**: objc シグネチャ見直し。Silk.NET SDL の API 名が `MetalCreateView`/`MetalGetLayer` と違う場合はコンパイルエラーになる — その場合は `grep -o 'MetalCreate[A-Za-z]*\|MetalGet[A-Za-z]*' ~/.nuget/packages/silk.net.sdl/2.22.0/lib/*/Silk.NET.SDL.xml | sort -u` で実名を確認して合わせる。

- [ ] **Step 5: Commit**

```bash
git add core/CefUnity.Viewer
git commit -m "feat: Viewer スパイク (SDL 窓 + Metal present + NSEvent/IME 疎通確認)"
```

---

### Task 3: CefFrameSource + GPU テクスチャ表示 (ブラウザが映る)

**Files:**
- Create: `core/CefUnity.Viewer/CefFrameSource.cs`
- Create: `core/CefUnity.Viewer/ViewerWindow.cs`
- Modify: `core/CefUnity.Viewer/Program.cs` (本実装に置き換え)

**Interfaces:**
- Consumes: `IFrameRenderer` / `MetalFrameRenderer` (Task 2)、`ViewerOptions` (Task 1)
- Produces: `CefFrameSource` — `CefFrameSource(int width, int height, string url)` / `Browser Browser { get; }` / `bool TickFrame(out IntPtr texturePointer, out int width, out int height)` (BeginFrame+Pump+受信。false = まだ 1 枚も来ていない) / `void Resize(int width, int height)` / `IDisposable`
- Produces: `ViewerWindow` — `ViewerWindow(ViewerOptions options, CefFrameSource frameSource)` / `void Run()` / `IDisposable`。以降のタスクはこのクラスにハンドラを足していく

- [ ] **Step 1: CefFrameSource を実装**

`core/CefUnity.Viewer/CefFrameSource.cs`:

```csharp
using CefUnity.Interop;

namespace CefUnity.Viewer
{
    /// <summary>CEF 側窓口: BeginFrame/Pump/IOSurface テクスチャ受信/リサイズ (spec §CefFrameSource)。</summary>
    internal sealed class CefFrameSource : IDisposable
    {
        private readonly Browser _browser;
        private IntPtr _currentTexture;
        private int _textureWidth;
        private int _textureHeight;
        private ulong _frameIndex;

        public CefFrameSource(int width, int height, string url)
        {
            _browser = new Browser(width, height, url);
        }

        public Browser Browser => _browser;

        /// <summary>毎フレーム 1 回。新フレームが無ければ直前のテクスチャを返し続ける。</summary>
        public bool TickFrame(out IntPtr texturePointer, out int width, out int height)
        {
            _browser.SendExternalBeginFrame(_frameIndex++);
            CefRuntime.Pump();
            if (Browser.TryReceiveIOSurfaceTexture(out var newTexture, out var newWidth, out var newHeight, out _))
            {
                if (_currentTexture != IntPtr.Zero) Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = newTexture;
                _textureWidth = newWidth;
                _textureHeight = newHeight;
            }
            texturePointer = _currentTexture;
            width = _textureWidth;
            height = _textureHeight;
            return _currentTexture != IntPtr.Zero;
        }

        /// <summary>server 側が was_resized + invalidate を行う (CLAUDE.md のリサイズ既知の罠は server 実装済み)。</summary>
        public void Resize(int width, int height) => _browser.Resize(width, height);

        public void Dispose()
        {
            if (_currentTexture != IntPtr.Zero)
            {
                Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = IntPtr.Zero;
            }
            _browser.Dispose();
        }
    }
}
```

- [ ] **Step 2: ViewerWindow を実装**

`core/CefUnity.Viewer/ViewerWindow.cs` (このタスクでは表示のみ。入力は後続タスクで追記):

```csharp
using Silk.NET.Maths;
using Silk.NET.SDL;
using Silk.NET.Windowing;
using Silk.NET.Windowing.Sdl;

namespace CefUnity.Viewer
{
    /// <summary>SDL 窓とフレームループの所有者。入力配線は後続タスクでこのクラスに追記する。</summary>
    internal sealed class ViewerWindow : IDisposable
    {
        private readonly ViewerOptions _options;
        private readonly CefFrameSource _frameSource;
        private readonly IWindow _window;
        private readonly Sdl _sdl;
        private readonly IFrameRenderer _renderer;
        private bool _firstFrameShown;

        public ViewerWindow(ViewerOptions options, CefFrameSource frameSource)
        {
            _options = options;
            _frameSource = frameSource;
            Window.PrioritizeSdl();
            _window = Silk.NET.Windowing.Window.Create(WindowOptions.Default with
            {
                API = GraphicsAPI.None,
                Size = new Vector2D<int>(options.Width, options.Height),
                Title = "CefUnity.Viewer (loading)",
                // ペーシングはタイマーではなく CAMetalLayer displaySync + nextDrawable ブロックに任せる
                FramesPerSecond = 0,
                UpdatesPerSecond = 0,
                VSync = false,
            });
            _sdl = SdlWindowing.GetExistingApi(_window)
                   ?? throw new InvalidOperationException("SDL backend not active");
            _renderer = new MetalFrameRenderer(_sdl);
            _window.Load += OnLoad;
            _window.Render += OnRender;
        }

        public void Run() => _window.Run();

        private void OnLoad()
        {
            _renderer.Initialize(_window);
        }

        private void OnRender(double deltaSeconds)
        {
            if (_frameSource.TickFrame(out var texturePointer, out var textureWidth, out var textureHeight))
            {
                _renderer.Present(texturePointer, textureWidth, textureHeight);
                if (!_firstFrameShown)
                {
                    _firstFrameShown = true;
                    _window.Title = "CefUnity.Viewer";
                }
            }
            else
            {
                // まだ 1 枚も来ていない: drawable だけ回す (黒画面)
                _renderer.Present(IntPtr.Zero, _options.Width, _options.Height);
            }
        }

        public void Dispose()
        {
            _renderer.Dispose();
            _window.Dispose();
        }
    }
}
```

- [ ] **Step 3: Program.cs を本実装に置き換え**

`core/CefUnity.Viewer/Program.cs` 全体:

```csharp
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
```

- [ ] **Step 4: 手動確認 — ブラウザが映る (S4 込み)**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet run --project core/CefUnity.Viewer -- --url https://example.com`
Expected: 窓に example.com が描画される。タイトルが "(loading)" → "CefUnity.Viewer" に変わる。窓を閉じるとプロセスが終了し `ps aux | grep cef-unity-server | grep -v grep` が空 (サーバー残留なし)。
S4 確認: ページ上で時計等が動く URL (`--url https://www.google.com`) でも描画更新が続く (CEF paint 凍結なし)。
NG の場合: `CefRuntime.GetLogs()` の出力と `Console.app` の server ログを確認。IOSurface 未接続が続く場合は `Browser.IsIOSurfaceConnected()` を OnRender でログして Mach 接続を切り分け。

- [ ] **Step 5: Commit**

```bash
git add core/CefUnity.Viewer
git commit -m "feat: Viewer で CEF GPU テクスチャ表示 (IOSurface→CAMetalLayer blit)"
```

---

### Task 4: マウス入力 (移動・クリック・raw ホイール)

**Files:**
- Create: `core/CefUnity.Viewer/ClickCounter.cs`
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs` (入力配線追加)
- Test: `core/CefUnity.Tests/ClickCounterTests.cs`

**Interfaces:**
- Consumes: `Browser.SendMouseMove/SendMouseClick/SendMouseWheel`、`MouseButton` (Interop)
- Produces: `ClickCounter` — `int OnMouseDown(double timestampSeconds, int x, int y)` (CEF の clickCount: ダブルクリック判定 500ms/4px)

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ClickCounterTests.cs`:

```csharp
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ClickCounterTests
    {
        [Test]
        public void OnMouseDown_QuickSamePlace_IncrementsClickCount()
        {
            var counter = new ClickCounter();
            Assert.That(counter.OnMouseDown(0.0, 100, 100), Is.EqualTo(1));
            Assert.That(counter.OnMouseDown(0.3, 101, 101), Is.EqualTo(2));
            Assert.That(counter.OnMouseDown(0.6, 100, 102), Is.EqualTo(3));
        }

        [Test]
        public void OnMouseDown_TooSlow_ResetsToSingle()
        {
            var counter = new ClickCounter();
            counter.OnMouseDown(0.0, 100, 100);
            Assert.That(counter.OnMouseDown(0.6, 100, 100), Is.EqualTo(1)); // 500ms 超
        }

        [Test]
        public void OnMouseDown_TooFar_ResetsToSingle()
        {
            var counter = new ClickCounter();
            counter.OnMouseDown(0.0, 100, 100);
            Assert.That(counter.OnMouseDown(0.1, 200, 100), Is.EqualTo(1)); // 4px 超
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter ClickCounterTests 2>&1 | tail -3`
Expected: FAIL (ClickCounter 未定義のコンパイルエラー)

- [ ] **Step 3: ClickCounter を実装**

`core/CefUnity.Viewer/ClickCounter.cs`:

```csharp
namespace CefUnity.Viewer
{
    /// <summary>CEF SendMouseClick の clickCount 算出 (500ms・4px 以内の連打で加算)。</summary>
    public sealed class ClickCounter
    {
        private const double MaxIntervalSeconds = 0.5;
        private const int MaxDistancePixels = 4;

        private double _lastTimestamp = double.NegativeInfinity;
        private int _lastX;
        private int _lastY;
        private int _clickCount;

        public int OnMouseDown(double timestampSeconds, int x, int y)
        {
            var withinTime = timestampSeconds - _lastTimestamp <= MaxIntervalSeconds;
            var withinDistance = Math.Abs(x - _lastX) <= MaxDistancePixels && Math.Abs(y - _lastY) <= MaxDistancePixels;
            _clickCount = withinTime && withinDistance ? _clickCount + 1 : 1;
            _lastTimestamp = timestampSeconds;
            _lastX = x;
            _lastY = y;
            return _clickCount;
        }
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter ClickCounterTests 2>&1 | tail -3`
Expected: PASS

- [ ] **Step 5: ViewerWindow に入力配線を追加**

`ViewerWindow.cs` に using `Silk.NET.Input;` と以下のフィールドを追加:

```csharp
        private IInputContext? _input;
        private IMouse? _mouse;
        private readonly ClickCounter _clickCounter = new ClickCounter();
        private int _mouseX;
        private int _mouseY;
        private double _elapsedSeconds;
```

`OnLoad()` の末尾に追加:

```csharp
            _input = _window.CreateInput();
            _mouse = _input.Mice.Count > 0 ? _input.Mice[0] : null;
            if (_mouse != null)
            {
                _mouse.MouseMove += OnMouseMove;
                _mouse.MouseDown += OnMouseDown;
                _mouse.MouseUp += OnMouseUp;
                _mouse.Scroll += OnMouseScroll;
            }
```

ハンドラ (クラス末尾に追加。modifiers は Task 5 で実装するまで 0):

```csharp
        private void OnMouseMove(IMouse mouse, System.Numerics.Vector2 position)
        {
            _mouseX = (int)position.X;
            _mouseY = (int)position.Y;
            _frameSource.Browser.SendMouseMove(_mouseX, _mouseY);
        }

        private static MouseButton ToCefMouseButton(Silk.NET.Input.MouseButton button) => button switch
        {
            Silk.NET.Input.MouseButton.Right => CefUnity.Interop.MouseButton.Right,
            Silk.NET.Input.MouseButton.Middle => CefUnity.Interop.MouseButton.Middle,
            _ => CefUnity.Interop.MouseButton.Left,
        };

        private void OnMouseDown(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            var clickCount = _clickCounter.OnMouseDown(_elapsedSeconds, _mouseX, _mouseY);
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: false, clickCount);
        }

        private void OnMouseUp(IMouse mouse, Silk.NET.Input.MouseButton button)
        {
            _frameSource.Browser.SendMouseClick(_mouseX, _mouseY, ToCefMouseButton(button), mouseUp: true);
        }

        private void OnMouseScroll(IMouse mouse, ScrollWheel wheel)
        {
            // Task 6 で ScrollInputMatrix に置き換える。まずは raw 直送 (Unity 旧経路相当)
            _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY,
                (int)(wheel.X * CefUnity.Runtime.ScrollInputPipeline.WheelPixelsPerStep),
                (int)(wheel.Y * CefUnity.Runtime.ScrollInputPipeline.WheelPixelsPerStep));
        }
```

`OnRender` の先頭に `_elapsedSeconds += deltaSeconds;` を追加。`Dispose()` の先頭に `_input?.Dispose();` を追加。
注意: `Silk.NET.Input.MouseButton` と `CefUnity.Interop.MouseButton` が衝突するため上記のように完全修飾する。

- [ ] **Step 6: 手動確認**

Run: `dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org`
Expected: リンクのホバー変化 (SendMouseMove)、クリックでページ遷移、ダブルクリックで単語選択、スクロールでページが動く (荒くて良い — raw)。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: Viewer マウス入力 (移動・クリック・raw ホイール)"
```

---

### Task 5: SilkKeyboardMapper + キーボード配線

**Files:**
- Create: `core/CefUnity.Viewer/SilkKeyboardMapper.cs`
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs`
- Test: `core/CefUnity.Tests/SilkKeyboardMapperTests.cs`

**Interfaces:**
- Consumes: `CefKeyCode` / `CefKeyCodes` / `CefEventFlags` / `KeyEventType` (Interop)
- Produces: `SilkKeyboardMapper` — `static bool TryMap(Silk.NET.Input.Key key, out CefKeyCode code)` / `static uint BuildModifiers(Silk.NET.Input.IKeyboard keyboard)`

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/SilkKeyboardMapperTests.cs`:

```csharp
using CefUnity.Interop;
using CefUnity.Viewer;
using NUnit.Framework;
using Silk.NET.Input;

namespace CefUnity.Tests
{
    [TestFixture]
    public class SilkKeyboardMapperTests
    {
        [TestCase(Key.A, 0x41, 0)]
        [TestCase(Key.Z, 0x5A, 6)]
        [TestCase(Key.Number0, 0x30, 29)]
        [TestCase(Key.Number1, 0x31, 18)]
        [TestCase(Key.Space, 0x20, 49)]
        public void TryMap_PrintableKey_ReturnsWindowsAndNativeCode(Key key, int expectedWindowsKeyCode, int expectedNativeKeyCode)
        {
            Assert.That(SilkKeyboardMapper.TryMap(key, out var code), Is.True);
            Assert.That(code.WindowsKeyCode, Is.EqualTo(expectedWindowsKeyCode));
            Assert.That(code.NativeKeyCode, Is.EqualTo(expectedNativeKeyCode));
        }

        [Test]
        public void TryMap_SpecialKeys_UseCefKeyCodesTable()
        {
            Assert.That(SilkKeyboardMapper.TryMap(Key.Enter, out var enter), Is.True);
            Assert.That(enter.WindowsKeyCode, Is.EqualTo(CefKeyCodes.Return.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.Backspace, out var backspace), Is.True);
            Assert.That(backspace.WindowsKeyCode, Is.EqualTo(CefKeyCodes.Backspace.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.Up, out var up), Is.True);
            Assert.That(up.WindowsKeyCode, Is.EqualTo(CefKeyCodes.UpArrow.WindowsKeyCode));
            Assert.That(SilkKeyboardMapper.TryMap(Key.PageDown, out var pageDown), Is.True);
            Assert.That(pageDown.WindowsKeyCode, Is.EqualTo(CefKeyCodes.PageDown.WindowsKeyCode));
        }

        [Test]
        public void TryMap_UnknownKey_ReturnsFalse()
        {
            Assert.That(SilkKeyboardMapper.TryMap(Key.Unknown, out _), Is.False);
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter SilkKeyboardMapperTests 2>&1 | tail -3`
Expected: FAIL (コンパイルエラー)

- [ ] **Step 3: SilkKeyboardMapper を実装**

`core/CefUnity.Viewer/SilkKeyboardMapper.cs` (mac ネイティブキーコードは kVK_* 標準表):

```csharp
using CefUnity.Interop;
using Silk.NET.Input;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     Silk.NET Key → CefKeyCode 変換。Unity 用 CefKeyboardMapper は UnityEngine.KeyCode
    ///     依存のため流用不可 (spec 参照)。文字入力自体は SDL TEXTINPUT (ImeBridge) が担い、
    ///     ここは物理キー (RawKeyDown/KeyUp) とショートカット用。
    /// </summary>
    public static class SilkKeyboardMapper
    {
        private static readonly Dictionary<Key, CefKeyCode> Table = BuildTable();

        public static bool TryMap(Key key, out CefKeyCode code) => Table.TryGetValue(key, out code);

        /// <summary>修飾キー状態を CefEventFlags に変換する (送信時に毎回呼ぶ)。</summary>
        public static uint BuildModifiers(IKeyboard keyboard)
        {
            var flags = CefEventFlags.None;
            if (keyboard.IsKeyPressed(Key.ShiftLeft) || keyboard.IsKeyPressed(Key.ShiftRight)) flags |= CefEventFlags.ShiftDown;
            if (keyboard.IsKeyPressed(Key.ControlLeft) || keyboard.IsKeyPressed(Key.ControlRight)) flags |= CefEventFlags.ControlDown;
            if (keyboard.IsKeyPressed(Key.AltLeft) || keyboard.IsKeyPressed(Key.AltRight)) flags |= CefEventFlags.AltDown;
            if (keyboard.IsKeyPressed(Key.SuperLeft) || keyboard.IsKeyPressed(Key.SuperRight)) flags |= CefEventFlags.CommandDown;
            return (uint)flags;
        }

        private static Dictionary<Key, CefKeyCode> BuildTable()
        {
            var table = new Dictionary<Key, CefKeyCode>
            {
                // 特殊キーは Core の CefKeyCodes 定義をそのまま使う
                [Key.Backspace] = CefKeyCodes.Backspace,
                [Key.Tab] = CefKeyCodes.Tab,
                [Key.Enter] = CefKeyCodes.Return,
                [Key.Escape] = CefKeyCodes.Escape,
                [Key.Delete] = CefKeyCodes.Delete,
                [Key.Insert] = CefKeyCodes.Insert,
                [Key.Up] = CefKeyCodes.UpArrow,
                [Key.Down] = CefKeyCodes.DownArrow,
                [Key.Left] = CefKeyCodes.LeftArrow,
                [Key.Right] = CefKeyCodes.RightArrow,
                [Key.Home] = CefKeyCodes.Home,
                [Key.End] = CefKeyCodes.End,
                [Key.PageUp] = CefKeyCodes.PageUp,
                [Key.PageDown] = CefKeyCodes.PageDown,
                [Key.F1] = CefKeyCodes.F1, [Key.F2] = CefKeyCodes.F2, [Key.F3] = CefKeyCodes.F3,
                [Key.F4] = CefKeyCodes.F4, [Key.F5] = CefKeyCodes.F5, [Key.F6] = CefKeyCodes.F6,
                [Key.F7] = CefKeyCodes.F7, [Key.F8] = CefKeyCodes.F8, [Key.F9] = CefKeyCodes.F9,
                [Key.F10] = CefKeyCodes.F10, [Key.F11] = CefKeyCodes.F11, [Key.F12] = CefKeyCodes.F12,
                [Key.Keypad0] = CefKeyCodes.Keypad0, [Key.Keypad1] = CefKeyCodes.Keypad1,
                [Key.Keypad2] = CefKeyCodes.Keypad2, [Key.Keypad3] = CefKeyCodes.Keypad3,
                [Key.Keypad4] = CefKeyCodes.Keypad4, [Key.Keypad5] = CefKeyCodes.Keypad5,
                [Key.Keypad6] = CefKeyCodes.Keypad6, [Key.Keypad7] = CefKeyCodes.Keypad7,
                [Key.Keypad8] = CefKeyCodes.Keypad8, [Key.Keypad9] = CefKeyCodes.Keypad9,
                [Key.KeypadDecimal] = CefKeyCodes.KeypadPeriod,
                [Key.KeypadDivide] = CefKeyCodes.KeypadDivide,
                [Key.KeypadMultiply] = CefKeyCodes.KeypadMultiply,
                [Key.KeypadSubtract] = CefKeyCodes.KeypadMinus,
                [Key.KeypadAdd] = CefKeyCodes.KeypadPlus,
                [Key.KeypadEnter] = CefKeyCodes.KeypadEnter,
                [Key.ShiftLeft] = CefKeyCodes.LeftShift, [Key.ShiftRight] = CefKeyCodes.RightShift,
                [Key.ControlLeft] = CefKeyCodes.LeftControl, [Key.ControlRight] = CefKeyCodes.RightControl,
                [Key.AltLeft] = CefKeyCodes.LeftAlt, [Key.AltRight] = CefKeyCodes.RightAlt,
                [Key.SuperLeft] = CefKeyCodes.LeftCommand, [Key.SuperRight] = CefKeyCodes.RightCommand,
                [Key.CapsLock] = CefKeyCodes.CapsLock,
                [Key.Space] = new CefKeyCode(0x20, 49, ' '),
                // 記号 (Windows VK_OEM_* / mac kVK_ANSI_*)
                [Key.Minus] = new CefKeyCode(0xBD, 27, '-'),
                [Key.Equal] = new CefKeyCode(0xBB, 24, '='),
                [Key.LeftBracket] = new CefKeyCode(0xDB, 33, '['),
                [Key.RightBracket] = new CefKeyCode(0xDD, 30, ']'),
                [Key.BackSlash] = new CefKeyCode(0xDC, 42, '\\'),
                [Key.Semicolon] = new CefKeyCode(0xBA, 41, ';'),
                [Key.Apostrophe] = new CefKeyCode(0xDE, 39, '\''),
                [Key.Comma] = new CefKeyCode(0xBC, 43, ','),
                [Key.Period] = new CefKeyCode(0xBE, 47, '.'),
                [Key.Slash] = new CefKeyCode(0xBF, 44, '/'),
                [Key.GraveAccent] = new CefKeyCode(0xC0, 50, '`'),
            };
            // 英字: windowsKeyCode は 'A'..'Z'、mac native は kVK_ANSI_* 標準表
            int[] letterNativeCodes =
            {
                0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46,      // A B C D E F G H I J K L M
                45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7, 16, 6,    // N O P Q R S T U V W X Y Z
            };
            for (var index = 0; index < 26; index++)
                table[Key.A + index] = new CefKeyCode('A' + index, letterNativeCodes[index], (char)('a' + index));
            // 数字: windowsKeyCode は '0'..'9'、mac native は kVK_ANSI_0..9
            int[] digitNativeCodes = { 29, 18, 19, 20, 21, 23, 22, 26, 28, 25 }; // 0 1 2 3 4 5 6 7 8 9
            for (var index = 0; index < 10; index++)
                table[Key.Number0 + index] = new CefKeyCode('0' + index, digitNativeCodes[index], (char)('0' + index));
            return table;
        }
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter SilkKeyboardMapperTests 2>&1 | tail -3`
Expected: PASS。`Key.A + index` / `Key.Number0 + index` の enum 連続性が Silk.NET で崩れていたらテストが落ちるので、その場合は明示テーブルに書き換える。

- [ ] **Step 5: ViewerWindow にキーボード配線**

`ViewerWindow.cs` にフィールド `private IKeyboard? _keyboard;` を追加し、`OnLoad()` の input 配線に追加:

```csharp
            _keyboard = _input.Keyboards.Count > 0 ? _input.Keyboards[0] : null;
            if (_keyboard != null)
            {
                _keyboard.KeyDown += OnKeyDown;
                _keyboard.KeyUp += OnKeyUp;
            }
```

ハンドラ追加 (F1/F2/F3/F5 は Task 6 で Viewer ショートカットとして消費する — ここではブラウザに送るだけ):

```csharp
        private void OnKeyDown(IKeyboard keyboard, Key key, int scanCode)
        {
            if (!SilkKeyboardMapper.TryMap(key, out var code)) return;
            _frameSource.Browser.SendKeyEvent(KeyEventType.RawKeyDown, code, SilkKeyboardMapper.BuildModifiers(keyboard));
        }

        private void OnKeyUp(IKeyboard keyboard, Key key, int scanCode)
        {
            if (!SilkKeyboardMapper.TryMap(key, out var code)) return;
            _frameSource.Browser.SendKeyEvent(KeyEventType.KeyUp, code, SilkKeyboardMapper.BuildModifiers(keyboard));
        }
```

using に `CefUnity.Interop;` を追加 (KeyEventType 用。MouseButton 衝突は Task 4 と同じく完全修飾で回避)。

- [ ] **Step 6: 手動確認**

Run: `dotnet run --project core/CefUnity.Viewer -- --url https://www.google.com`
Expected: 検索欄クリック→英数タイプで文字が入る (SDL TEXTINPUT は未配線だが RawKeyDown で Google は反応しない — **文字が入らないのは正常**。矢印キー/PageDown でページスクロール、Tab でフォーカス移動が動くことを確認)。文字入力の成立は Task 9 (ImeBridge) 後。
cmd+C / cmd+V が効かない場合は Task 9 の後に `EditCommand` 配線を検討する (spec のエラー処理外、必要になったときのみ)。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: Viewer キーボード入力 (SilkKeyboardMapper + RawKeyDown/KeyUp 配線)"
```

---

### Task 6: ScrollInputMatrix (3 モード切替 + native ソース + 録画)

**Files:**
- Create: `core/CefUnity.Viewer/ScrollInputMatrix.cs`
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs`
- Test: `core/CefUnity.Tests/ScrollInputMatrixTests.cs`

**Interfaces:**
- Consumes: `ScrollInputPipeline` / `IScrollEventSource` / `ScrollInputEvent` / `NativeScrollSourceStart` (Core)
- Produces: `ScrollInputMatrix` —
  - `ScrollMode Mode { get; }` / `void SetMode(ScrollMode mode)` (切替時に pipeline.Reset + raw 蓄積クリア)
  - `NativeScrollSourceStart StartNativeSource(out Exception? error)` / `void AttachSource(IScrollEventSource source)` (リプレイ用)
  - `void AddWheelSteps(float xSteps, float ySteps)` (窓 wheel イベント。Raw=直送蓄積 / Smoother=スムーザ / Resampler=無視)
  - `void TickFrame(float deltaTimeSeconds, bool overBrowser, out int primaryDeltaX, out int primaryDeltaY, out int secondaryDeltaX, out int secondaryDeltaY)` — 呼び出し側は primary→secondary の順で非 0 のものを SendMouseWheel する (Pipeline の「Drain → TickResampler → 送信 → TickSmoother → 送信」順序を 2 送信で保つ)
  - `bool RecordingEnabled { get; set; }` / `IDisposable`

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ScrollInputMatrixTests.cs` (native ソースの代わりに fake source を Attach):

```csharp
using CefUnity.Runtime;
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollInputMatrixTests
    {
        /// <summary>時刻を手動で進められる fake ソース。</summary>
        private sealed class FakeScrollSource : IScrollEventSource
        {
            public readonly List<ScrollInputEvent> Pending = new();
            public double CurrentTime;
            public bool Start() => true;
            public double Now => CurrentTime;
            public int Poll(ScrollInputEvent[] buffer)
            {
                var count = Math.Min(Pending.Count, buffer.Length);
                for (var index = 0; index < count; index++) buffer[index] = Pending[index];
                Pending.RemoveRange(0, count);
                return count;
            }
            public void Dispose() { }
        }

        [Test]
        public void RawMode_WheelSteps_EmitOncePerFrame()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Raw);
            matrix.AddWheelSteps(0f, -1f);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out var secondaryDeltaY);
            Assert.That(primaryDeltaY, Is.EqualTo((int)(-1f * ScrollInputPipeline.WheelPixelsPerStep)));
            Assert.That(secondaryDeltaY, Is.EqualTo(0));
            // 消費済み: 次フレームは 0
            matrix.TickFrame(0.016f, overBrowser: true, out _, out primaryDeltaY, out _, out _);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
        }

        [Test]
        public void SmootherMode_WheelSteps_GlideOverFrames()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Smoother);
            matrix.AddWheelSteps(0f, -1f);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var firstDeltaY, out _, out _);
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var secondDeltaY, out _, out _);
            Assert.That(firstDeltaY, Is.Not.EqualTo(0));
            Assert.That(Math.Abs(firstDeltaY), Is.LessThan(60)); // 一括 60px でなく分散排出
            Assert.That(secondDeltaY, Is.Not.EqualTo(0));
        }

        [Test]
        public void ResamplerMode_PreciseEvents_FlowThroughResampler()
        {
            using var matrix = new ScrollInputMatrix();
            var source = new FakeScrollSource();
            matrix.AttachSource(source);
            matrix.SetMode(ScrollMode.Resampler);
            // 8ms 間隔の precise イベント 3 連 (60Hz NSEvent 相当)
            for (var index = 0; index < 3; index++)
                source.Pending.Add(new ScrollInputEvent
                {
                    Timestamp = index * 0.008, DeltaYPixels = -10f, Precise = true, Phase = ScrollPhase.GestureChanged,
                });
            source.CurrentTime = 0.030;
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out _);
            source.CurrentTime = 0.046;
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var nextDeltaY, out _, out _);
            Assert.That(primaryDeltaY + nextDeltaY, Is.LessThan(0)); // 下方向の排出が発生
        }

        [Test]
        public void ResamplerMode_WindowWheelSteps_Ignored()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.AttachSource(new FakeScrollSource());
            matrix.SetMode(ScrollMode.Resampler);
            matrix.AddWheelSteps(0f, -1f); // native と二重計上しない
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out var secondaryDeltaY);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
            Assert.That(secondaryDeltaY, Is.EqualTo(0));
        }

        [Test]
        public void SetMode_ClearsPendingState()
        {
            using var matrix = new ScrollInputMatrix();
            matrix.SetMode(ScrollMode.Raw);
            matrix.AddWheelSteps(0f, -5f);
            matrix.SetMode(ScrollMode.Smoother); // 切替で raw 蓄積を破棄
            matrix.TickFrame(0.016f, overBrowser: true, out _, out var primaryDeltaY, out _, out _);
            Assert.That(primaryDeltaY, Is.EqualTo(0));
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollInputMatrixTests 2>&1 | tail -3`
Expected: FAIL (コンパイルエラー)

- [ ] **Step 3: ScrollInputMatrix を実装**

`core/CefUnity.Viewer/ScrollInputMatrix.cs`:

```csharp
using CefUnity.Runtime;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     スクロール 3 モードの切替と毎フレーム排出 (spec §ScrollInputMatrix)。
    ///     ①Raw: 窓 wheel を 1:1 直送 (平滑化前の旧 Unity 相当)
    ///     ②Smoother: 窓 wheel → ScrollSmoother (Unity A案)
    ///     ③Resampler: native/replay ソース → ScrollResampler 予測 (Unity C案 = 現行既定)
    ///     native ソースの Drain は全モードで行う (録画のため) が、転送は Resampler モードのみ。
    /// </summary>
    public sealed class ScrollInputMatrix : IDisposable
    {
        private readonly ScrollInputPipeline _pipeline = new ScrollInputPipeline();
        private float _rawPendingX;
        private float _rawPendingY;

        public ScrollMode Mode { get; private set; } = ScrollMode.Resampler;

        public NativeScrollSourceStart StartNativeSource(out Exception? error) => _pipeline.StartNativeSource(out error!);

        public void AttachSource(IScrollEventSource source) => _pipeline.AttachSource(source);

        public bool RecordingEnabled
        {
            get => _pipeline.RecordingEnabled;
            set => _pipeline.RecordingEnabled = value;
        }

        public void SetMode(ScrollMode mode)
        {
            Mode = mode;
            _pipeline.Reset();
            _rawPendingX = 0f;
            _rawPendingY = 0f;
        }

        public void AddWheelSteps(float xSteps, float ySteps)
        {
            switch (Mode)
            {
                case ScrollMode.Raw:
                    _rawPendingX += xSteps * ScrollInputPipeline.WheelPixelsPerStep;
                    _rawPendingY += ySteps * ScrollInputPipeline.WheelPixelsPerStep;
                    break;
                case ScrollMode.Smoother:
                    _pipeline.AddWheelSteps(xSteps, ySteps, resolutionScale: 1f);
                    break;
                // Resampler: 窓 wheel は無視 (native ソースが同じ物理イベントを拾う — 二重計上防止)
            }
        }

        public void TickFrame(float deltaTimeSeconds, bool overBrowser,
            out int primaryDeltaX, out int primaryDeltaY, out int secondaryDeltaX, out int secondaryDeltaY)
        {
            primaryDeltaX = 0;
            primaryDeltaY = 0;
            secondaryDeltaX = 0;
            secondaryDeltaY = 0;
            // 順序は Pipeline 規約: Drain → TickResampler → (送信) → TickSmoother → (送信)
            _pipeline.Drain(overBrowser && Mode == ScrollMode.Resampler, resolutionScale: 1f);
            switch (Mode)
            {
                case ScrollMode.Resampler:
                    _pipeline.TickResampler(out primaryDeltaX, out primaryDeltaY);
                    // 非 precise (ホイールノッチ) は Drain がスムーザへ回すので secondary で排出
                    _pipeline.TickSmoother(deltaTimeSeconds, out secondaryDeltaX, out secondaryDeltaY);
                    break;
                case ScrollMode.Smoother:
                    _pipeline.TickSmoother(deltaTimeSeconds, out primaryDeltaX, out primaryDeltaY);
                    break;
                case ScrollMode.Raw:
                    primaryDeltaX = ConsumeWhole(ref _rawPendingX);
                    primaryDeltaY = ConsumeWhole(ref _rawPendingY);
                    break;
            }
        }

        private static int ConsumeWhole(ref float pending)
        {
            var whole = (int)pending;
            pending -= whole;
            return whole;
        }

        public void Dispose() => _pipeline.Dispose();
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollInputMatrixTests 2>&1 | tail -3`
Expected: PASS

- [ ] **Step 5: ViewerWindow を ScrollInputMatrix に切り替え + ショートカット**

`ViewerWindow.cs`:
- `using CefUnity.Runtime;` を追加 (NativeScrollSourceStart 用)
- コンストラクタを `ViewerWindow(ViewerOptions options, CefFrameSource frameSource, ScrollInputMatrix scrollMatrix)` に変更し、フィールド `private readonly ScrollInputMatrix _scrollMatrix;` を追加
- `OnLoad()` 末尾に native ソース開始を追加 (リプレイ時は Program が AttachSource 済みのため呼ばない — `_scrollMatrix` に既にソースがある場合の再 Attach は StartNativeSource が上書きしてしまうので、`options.ReplayPath == null` のときだけ):

```csharp
            if (_options.ReplayPath == null)
            {
                var startResult = _scrollMatrix.StartNativeSource(out var startError);
                if (startResult != NativeScrollSourceStart.Started)
                    Console.WriteLine($"native scroll source: {startResult} {startError?.Message} — フォールバック (窓 wheel イベント)");
            }
            _scrollMatrix.SetMode(_options.Mode);
            _scrollMatrix.RecordingEnabled = _options.Record;
```

- `OnMouseScroll` を置き換え:

```csharp
        private void OnMouseScroll(IMouse mouse, ScrollWheel wheel)
        {
            _scrollMatrix.AddWheelSteps(wheel.X, wheel.Y);
        }
```

- `OnRender` の TickFrame の**前**に排出処理を追加 (spec のループ順序: スクロール → 入力 → BeginFrame):

```csharp
            var overBrowser = _mouseX >= 0 && _mouseY >= 0
                              && _mouseX < _window.Size.X && _mouseY < _window.Size.Y;
            _scrollMatrix.TickFrame((float)deltaSeconds, overBrowser,
                out var primaryDeltaX, out var primaryDeltaY, out var secondaryDeltaX, out var secondaryDeltaY);
            if (primaryDeltaX != 0 || primaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, primaryDeltaX, primaryDeltaY);
            if (secondaryDeltaX != 0 || secondaryDeltaY != 0)
                _frameSource.Browser.SendMouseWheel(_mouseX, _mouseY, secondaryDeltaX, secondaryDeltaY);
            _lastSentDeltaY = primaryDeltaY + secondaryDeltaY; // Task 8 の統計用
```

フィールド `private int _lastSentDeltaY;` を追加。

- `OnKeyDown` の先頭にショートカット消費を追加 (ブラウザに送らない):

```csharp
            switch (key)
            {
                case Key.F1: _scrollMatrix.SetMode(ScrollMode.Raw); UpdateTitle(); return;
                case Key.F2: _scrollMatrix.SetMode(ScrollMode.Smoother); UpdateTitle(); return;
                case Key.F3: _scrollMatrix.SetMode(ScrollMode.Resampler); UpdateTitle(); return;
                case Key.F5:
                    _scrollMatrix.RecordingEnabled = !_scrollMatrix.RecordingEnabled;
                    UpdateTitle();
                    return;
            }
```

- タイトル更新メソッドを追加し、`OnRender` の `_firstFrameShown` 分岐の `_window.Title = "CefUnity.Viewer";` を `UpdateTitle();` に置き換え:

```csharp
        private void UpdateTitle()
        {
            var recording = _scrollMatrix.RecordingEnabled ? " REC" : "";
            _window.Title = $"CefUnity.Viewer [{_scrollMatrix.Mode}]{recording}";
        }
```

- `Program.cs` の using ブロックを更新:

```csharp
    using var frameSource = new CefFrameSource(viewerOptions.Width, viewerOptions.Height, viewerOptions.Url);
    using var scrollMatrix = new ScrollInputMatrix();
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource, scrollMatrix);
    viewerWindow.Run();
```

- [ ] **Step 6: 全テスト + 手動確認**

Run: `dotnet test core/CefUnity.Tests 2>&1 | tail -3` → PASS (全緑)
Run: `dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org/wiki/%E7%8C%AB`
Expected: タイトルに `[Resampler]`。トラックパッドスクロールが滑らか (Unity C案相当)。F1 で `[Raw]` になり明確にカクつく。F2 で `[Smoother]`。F5 で ` REC` が付き、スクロール後に `$TMPDIR/cef_scroll_events.csv` に E/T 行が増える (`wc -l $TMPDIR/cef_scroll_events.csv`)。
**これが スパイク S1 の本番確認**: `[Resampler]` でスクロールが動かない/native scroll source が Started 以外 → NSEvent monitor が SDL で不成立 → 設計に戻る。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: Viewer スクロール 3 モードマトリクス (raw/smoother/resampler + 録画)"
```

---

### Task 7: ScrollReplaySource (Core) + --replay 配線

**Files:**
- Create: `core/CefUnity.Core/ScrollInput/ScrollReplaySource.cs`
- Modify: `core/CefUnity.Viewer/Program.cs`
- Test: `core/CefUnity.Tests/ScrollReplaySourceTests.cs`

**Interfaces:**
- Consumes: `IScrollEventSource` / `ScrollInputEvent` (Core)、録画 CSV 形式 `S,scale` / `E,ts,dx,dy,phase,precise,over`
- Produces: `CefUnity.Runtime.ScrollReplaySource` — `ScrollReplaySource(IEnumerable<string> csvLines, Func<double>? clock = null)` (clock は単調秒。省略時 Stopwatch) / `IScrollEventSource` 実装 / `bool Finished { get; }` / `int TotalEvents { get; }`。E 行のうち over=1 のみ採用・S 行の scale を delta に乗算・録画タイムラインを Start() 時点の clock に写像して実時間再生する

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ScrollReplaySourceTests.cs`:

```csharp
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollReplaySourceTests
    {
        private static readonly string[] SampleLines =
        {
            "S,1",
            "E,100.000,0,-10,2,1,1",
            "E,100.016,0,-12,2,1,1",
            "E,100.900,0,-99,2,1,0",   // over=0 → 除外
            "T,100.016,0,-10,1",        // T 行は無視
            "E,101.000,0,-3,5,1,1",
        };

        [Test]
        public void Poll_AdvancesWithClock_EmitsEventsInRecordedOrder()
        {
            var clockSeconds = 50.0;
            var source = new ScrollReplaySource(SampleLines, () => clockSeconds);
            Assert.That(source.Start(), Is.True);
            Assert.That(source.TotalEvents, Is.EqualTo(3));

            var buffer = new ScrollInputEvent[16];
            // 開始直後 (録画時刻 100.000 相当): 最初のイベントのみ
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-10f));
            Assert.That(buffer[0].Timestamp, Is.EqualTo(100.000).Within(1e-9));

            clockSeconds = 50.020; // 録画時刻 100.020 相当
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-12f));
            Assert.That(source.Finished, Is.False);

            clockSeconds = 51.100; // 録画時刻 101.100 相当 (over=0 は飛ばして最後の 1 件)
            Assert.That(source.Poll(buffer), Is.EqualTo(1));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-3f));
            Assert.That(source.Finished, Is.True);
        }

        [Test]
        public void Now_MapsToRecordedTimeline()
        {
            var clockSeconds = 50.0;
            var source = new ScrollReplaySource(SampleLines, () => clockSeconds);
            source.Start();
            Assert.That(source.Now, Is.EqualTo(100.000).Within(1e-9));
            clockSeconds = 50.5;
            Assert.That(source.Now, Is.EqualTo(100.500).Within(1e-9));
        }

        [Test]
        public void ScaleRow_MultipliesDeltas()
        {
            var clockSeconds = 0.0;
            var source = new ScrollReplaySource(new[] { "S,2", "E,10.0,1,-10,2,1,1" }, () => clockSeconds);
            source.Start();
            var buffer = new ScrollInputEvent[4];
            source.Poll(buffer);
            Assert.That(buffer[0].DeltaXPixels, Is.EqualTo(2f));
            Assert.That(buffer[0].DeltaYPixels, Is.EqualTo(-20f));
        }

        [Test]
        public void EmptyRecording_StartReturnsFalse()
        {
            var source = new ScrollReplaySource(new[] { "T,1.0,0,0,1" }, () => 0.0);
            Assert.That(source.Start(), Is.False);
        }

        [Test]
        public void ExistingFixture_LoadsAllForwardedEvents()
        {
            var lines = File.ReadAllLines(Path.Combine(TestContext.CurrentContext.TestDirectory,
                "fixtures", "cef_scroll_events_nozerowait.csv"));
            var source = new ScrollReplaySource(lines, () => 0.0);
            Assert.That(source.Start(), Is.True);
            Assert.That(source.TotalEvents, Is.GreaterThan(100));
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollReplaySourceTests 2>&1 | tail -3`
Expected: FAIL (コンパイルエラー)

- [ ] **Step 3: ScrollReplaySource を実装**

`core/CefUnity.Core/ScrollInput/ScrollReplaySource.cs`:

```csharp
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     cef_scroll_record の E 行 (over=1) を録画タイムラインどおりにライブ再生する
    ///     IScrollEventSource。Unity で録った入力を Viewer の生きた CEF に流し、
    ///     ホスト間比較 (spec の実験プロトコル) を行うための供給源。
    ///     ScrollReplayRunner (オフライン忠実度照合) とは役割が異なる。
    /// </summary>
    public sealed class ScrollReplaySource : IScrollEventSource
    {
        private readonly List<ScrollInputEvent> _events = new List<ScrollInputEvent>();
        private readonly Func<double> _clock;
        private double _clockStart;
        private double _timelineStart;
        private int _cursor;
        private bool _started;

        public ScrollReplaySource(IEnumerable<string> csvLines, Func<double>? clock = null)
        {
            _clock = clock ?? DefaultClock;
            var scale = 1f;
            foreach (var line in csvLines)
            {
                if (line.Length == 0) continue;
                var columns = line.Split(',');
                if (columns.Length >= 2 && columns[0] == "S")
                {
                    scale = float.Parse(columns[1], CultureInfo.InvariantCulture);
                }
                else if (columns.Length >= 7 && columns[0] == "E" && columns[6] == "1")
                {
                    _events.Add(new ScrollInputEvent
                    {
                        Timestamp = double.Parse(columns[1], CultureInfo.InvariantCulture),
                        DeltaXPixels = float.Parse(columns[2], CultureInfo.InvariantCulture) * scale,
                        DeltaYPixels = float.Parse(columns[3], CultureInfo.InvariantCulture) * scale,
                        Phase = (ScrollPhase)byte.Parse(columns[4], CultureInfo.InvariantCulture),
                        Precise = columns[5] == "1",
                    });
                }
            }
        }

        public int TotalEvents => _events.Count;

        public bool Finished => _cursor >= _events.Count;

        public bool Start()
        {
            if (_events.Count == 0) return false;
            _clockStart = _clock();
            _timelineStart = _events[0].Timestamp;
            _started = true;
            return true;
        }

        public double Now => _timelineStart + (_clock() - _clockStart);

        public int Poll(ScrollInputEvent[] buffer)
        {
            if (!_started) return 0;
            var now = Now;
            var count = 0;
            while (_cursor < _events.Count && count < buffer.Length && _events[_cursor].Timestamp <= now)
                buffer[count++] = _events[_cursor++];
            return count;
        }

        public void Dispose() { }

        private static double DefaultClock() => Stopwatch.GetTimestamp() / (double)Stopwatch.Frequency;
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollReplaySourceTests 2>&1 | tail -3`
Expected: PASS

- [ ] **Step 5: Program.cs に --replay 配線**

`Program.cs` の using ブロックを更新 (`using CefUnity.Runtime;` を追加):

```csharp
    using var frameSource = new CefFrameSource(viewerOptions.Width, viewerOptions.Height, viewerOptions.Url);
    using var scrollMatrix = new ScrollInputMatrix();
    if (viewerOptions.ReplayPath != null)
    {
        var replaySource = new ScrollReplaySource(File.ReadLines(viewerOptions.ReplayPath));
        if (!replaySource.Start())
        {
            Console.Error.WriteLine($"replay: {viewerOptions.ReplayPath} に over=1 の E 行がない");
            return 2;
        }
        Console.WriteLine($"replay: {replaySource.TotalEvents} events");
        scrollMatrix.AttachSource(replaySource);
    }
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource, scrollMatrix);
    viewerWindow.Run();
```

(ViewerWindow 側は Task 6 Step 5 で `ReplayPath == null` のときだけ StartNativeSource する分岐を入れてある)

- [ ] **Step 6: 手動確認 — 録画→リプレイの通し**

```
1. dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org/wiki/%E7%8C%AB --record
   → 窓上でトラックパッドスクロール数回 → 窓を閉じる
2. cp $TMPDIR/cef_scroll_events.csv /tmp/viewer_recording.csv
3. dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org/wiki/%E7%8C%AB --replay /tmp/viewer_recording.csv
```
Expected: 手を触れずに録画どおりのスクロールが再生される (⚠️ 計測の罠: リプレイ観察中は窓を最前面アクティブに保つ — 非アクティブだと CEF paint が凍結する)。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Core core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: ScrollReplaySource (録画のライブ再生) と Viewer --replay"
```

---

### Task 8: ScrollRoughnessAnalyzer (Core) + StatisticsRecorder + --statistics/--analyze

**Files:**
- Create: `core/CefUnity.Core/Scroll/ScrollRoughnessAnalyzer.cs`
- Create: `core/CefUnity.Viewer/StatisticsRecorder.cs`
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs` / `core/CefUnity.Viewer/Program.cs`
- Test: `core/CefUnity.Tests/ScrollRoughnessAnalyzerTests.cs`

**Interfaces:**
- Produces: `CefUnity.Runtime.ScrollRoughnessAnalyzer` — `static double ComputeRoughness(IReadOnlyList<int> sentDeltaY)`。**定義: 隣接フレームペア (d[i-1], d[i]) のうちどちらかが非 0 のものについて、roughness = Σ|d[i]−d[i−1]| / Σ|d[i]| (分母は活動フレームの総移動量)。0 = 完全均一。** 過去の Unity 実測 0.147/0.088 とは定義が同一とは限らないため、ホスト間比較は必ず本関数で両 CSV を再計算して行う (spec の粗さ指標節)
- Produces: `CefUnity.Viewer.StatisticsRecorder` — `StatisticsRecorder(string path)` / `void RecordFrame(long frameIndex, double deltaTimeMilliseconds, ulong paintFrameId, int sentDeltaX, int sentDeltaY, ScrollMode mode)` / `IDisposable` (flush)。CSV ヘッダ: `frame,dt_milliseconds,paint_frame_id,sent_delta_x,sent_delta_y,mode`

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ScrollRoughnessAnalyzerTests.cs`:

```csharp
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ScrollRoughnessAnalyzerTests
    {
        [Test]
        public void ComputeRoughness_PerfectlyUniform_ReturnsZero()
        {
            var roughness = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -10, -10, -10, -10, 0 });
            // 差が出るのは立ち上がり/立ち下がりの 2 遷移 (10+10) のみ、分母は総移動量 40。
            // 定常区間 (-10→-10) の差 0 が長いほど 0 に近づく (次テスト参照)
            Assert.That(roughness, Is.EqualTo(20.0 / 40.0).Within(1e-9));
        }

        [Test]
        public void ComputeRoughness_LongUniformRun_ApproachesZero()
        {
            var deltas = new int[102];
            for (var index = 1; index <= 100; index++) deltas[index] = -10;
            var roughness = ScrollRoughnessAnalyzer.ComputeRoughness(deltas);
            Assert.That(roughness, Is.EqualTo(20.0 / 1000.0).Within(1e-9)); // 端 2 遷移のみ
        }

        [Test]
        public void ComputeRoughness_Jittery_IsHigherThanUniform()
        {
            var uniform = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -10, -10, -10, -10, -10, -10, 0 });
            var jittery = ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, -2, -18, -4, -16, -6, -14, 0 });
            Assert.That(jittery, Is.GreaterThan(uniform));
        }

        [Test]
        public void ComputeRoughness_NoScroll_ReturnsZero()
        {
            Assert.That(ScrollRoughnessAnalyzer.ComputeRoughness(new[] { 0, 0, 0 }), Is.EqualTo(0.0));
            Assert.That(ScrollRoughnessAnalyzer.ComputeRoughness(new int[0]), Is.EqualTo(0.0));
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollRoughnessAnalyzerTests 2>&1 | tail -3`
Expected: FAIL (コンパイルエラー)

- [ ] **Step 3: ScrollRoughnessAnalyzer を実装**

`core/CefUnity.Core/Scroll/ScrollRoughnessAnalyzer.cs`:

```csharp
using System;
using System.Collections.Generic;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     フレーム毎スクロール排出量の「粗さ」指標。
    ///     roughness = Σ|d[i]−d[i−1]| / Σ|d[i]|
    ///     対象は隣接ペアのどちらかが非 0 の遷移 (分子) と非 0 フレーム (分母)。
    ///     0 = 完全均一。ホスト間比較 (Unity vs Viewer) は必ず本関数で両者を計算する
    ///     (過去のアドホック集計値 0.147/0.088 とは定義互換を保証しない)。
    /// </summary>
    public static class ScrollRoughnessAnalyzer
    {
        public static double ComputeRoughness(IReadOnlyList<int> sentDeltaY)
        {
            double transitionSum = 0;
            double magnitudeSum = 0;
            for (var index = 0; index < sentDeltaY.Count; index++)
            {
                magnitudeSum += Math.Abs(sentDeltaY[index]);
                if (index == 0) continue;
                var previous = sentDeltaY[index - 1];
                var current = sentDeltaY[index];
                if (previous != 0 || current != 0)
                    transitionSum += Math.Abs(current - previous);
            }
            return magnitudeSum == 0 ? 0.0 : transitionSum / magnitudeSum;
        }
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollRoughnessAnalyzerTests 2>&1 | tail -3`
Expected: PASS

- [ ] **Step 5: StatisticsRecorder と配線を実装**

`core/CefUnity.Viewer/StatisticsRecorder.cs`:

```csharp
namespace CefUnity.Viewer
{
    /// <summary>フレーム毎の計測 CSV (spec §StatisticsRecorder)。paint fps は paint_frame_id の差分から後段解析。</summary>
    internal sealed class StatisticsRecorder : IDisposable
    {
        public const string Header = "frame,dt_milliseconds,paint_frame_id,sent_delta_x,sent_delta_y,mode";

        private readonly StreamWriter _writer;

        public StatisticsRecorder(string path)
        {
            _writer = new StreamWriter(path);
            _writer.WriteLine(Header);
        }

        public void RecordFrame(long frameIndex, double deltaTimeMilliseconds, ulong paintFrameId,
            int sentDeltaX, int sentDeltaY, ScrollMode mode)
        {
            _writer.WriteLine(FormattableString.Invariant(
                $"{frameIndex},{deltaTimeMilliseconds:F3},{paintFrameId},{sentDeltaX},{sentDeltaY},{mode}"));
        }

        public void Dispose() => _writer.Dispose();
    }
}
```

`ViewerWindow.cs`: コンストラクタを `ViewerWindow(ViewerOptions options, CefFrameSource frameSource, ScrollInputMatrix scrollMatrix, StatisticsRecorder? statistics)` に変更、フィールド `private readonly StatisticsRecorder? _statistics; private long _statisticsFrameIndex; private int _lastSentDeltaX;` を追加。`OnRender` の排出処理で `_lastSentDeltaX = primaryDeltaX + secondaryDeltaX;` も保存し、`OnRender` 末尾に追加:

```csharp
            _statistics?.RecordFrame(_statisticsFrameIndex++, deltaSeconds * 1000.0,
                _frameSource.Browser.PeekAcceleratedFrameId(), _lastSentDeltaX, _lastSentDeltaY, _scrollMatrix.Mode);
```

`Program.cs`: ViewerOptions 解析直後 (CefRuntime.Initialize の**前**) に --analyze 分岐を追加:

```csharp
if (viewerOptions.AnalyzePath != null)
{
    var sentDeltaY = new List<int>();
    foreach (var line in File.ReadLines(viewerOptions.AnalyzePath).Skip(1))
    {
        var columns = line.Split(',');
        if (columns.Length >= 5) sentDeltaY.Add(int.Parse(columns[4]));
    }
    var roughness = CefUnity.Runtime.ScrollRoughnessAnalyzer.ComputeRoughness(sentDeltaY);
    Console.WriteLine(FormattableString.Invariant($"frames={sentDeltaY.Count} roughness={roughness:F4}"));
    return 0;
}
```

using ブロックに statistics を追加:

```csharp
    using var statistics = viewerOptions.StatisticsPath != null
        ? new StatisticsRecorder(viewerOptions.StatisticsPath) : null;
    using var viewerWindow = new ViewerWindow(viewerOptions, frameSource, scrollMatrix, statistics);
```

- [ ] **Step 6: 全テスト + 手動確認 (実験プロトコルの通し)**

Run: `dotnet test core/CefUnity.Tests 2>&1 | tail -3` → PASS
手動:

```
1. dotnet run --project core/CefUnity.Viewer -- --replay /tmp/viewer_recording.csv --statistics /tmp/viewer_statistics.csv
   (Task 7 の録画を使用。再生終了まで窓をアクティブに保ち、終わったら閉じる)
2. dotnet run --project core/CefUnity.Viewer -- --analyze /tmp/viewer_statistics.csv
```
Expected: `frames=N roughness=0.xxxx` が出る。同一録画を `--scroll-mode raw` 相当で流す比較はまだできない (リプレイは resampler 経路固定) が、live raw (F1) で録った statistics と resampler (F3) の statistics で roughness が **raw > resampler** になることを確認。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Core core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: 粗さ指標 (ScrollRoughnessAnalyzer) と Viewer 統計 CSV/解析"
```

---

### Task 9: ImeBridge (日本語入力)

**Files:**
- Create: `core/CefUnity.Viewer/ImeBridge.cs`
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs`
- Test: `core/CefUnity.Tests/ImeBridgeTests.cs`

**Interfaces:**
- Consumes: `Browser.ImeSetComposition/ImeCommitText/ImeFinishComposingText/ImeCancelComposition/SendCharEvent/GetImeCaret` (Interop)、SDL `TEXTEDITING`/`TEXTINPUT` イベント (Task 2 の AddEventWatch パターン)
- Produces: `CefUnity.Viewer.IImeSink` — `void SetComposition(string text, uint cursorPosition)` / `void CommitText(string text)` / `void SendCharacter(char character)` / `void FinishComposition()` / `void CancelComposition()`
- Produces: `CefUnity.Viewer.ImeBridge` — `ImeBridge(IImeSink sink)` / `void OnTextEditing(string text, int cursorStart)` / `void OnTextInput(string text)` / `void OnFocusLost()` / `bool Composing { get; }`

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ImeBridgeTests.cs`:

```csharp
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class ImeBridgeTests
    {
        private sealed class RecordingSink : IImeSink
        {
            public readonly List<string> Calls = new();
            public void SetComposition(string text, uint cursorPosition) => Calls.Add($"set:{text}:{cursorPosition}");
            public void CommitText(string text) => Calls.Add($"commit:{text}");
            public void SendCharacter(char character) => Calls.Add($"char:{character}");
            public void FinishComposition() => Calls.Add("finish");
            public void CancelComposition() => Calls.Add("cancel");
        }

        [Test]
        public void AsciiTyping_NoComposition_SendsCharEvents()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextInput("ab");
            Assert.That(sink.Calls, Is.EqualTo(new[] { "char:a", "char:b" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void JapaneseComposition_EditThenCommit()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnTextEditing("かん", 2);
            bridge.OnTextInput("漢");
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "set:かん:2", "commit:漢" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void EmptyEditingDuringComposition_Cancels()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnTextEditing("", 0); // Esc で変換破棄
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "cancel" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void FocusLostDuringComposition_Finishes()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnTextEditing("か", 1);
            bridge.OnFocusLost();
            Assert.That(sink.Calls, Is.EqualTo(new[] { "set:か:1", "finish" }));
            Assert.That(bridge.Composing, Is.False);
        }

        [Test]
        public void FocusLostWithoutComposition_DoesNothing()
        {
            var sink = new RecordingSink();
            var bridge = new ImeBridge(sink);
            bridge.OnFocusLost();
            Assert.That(sink.Calls, Is.Empty);
        }
    }
}
```

- [ ] **Step 2: テスト失敗を確認**

Run: `dotnet test core/CefUnity.Tests --filter ImeBridgeTests 2>&1 | tail -3`
Expected: FAIL (コンパイルエラー)

- [ ] **Step 3: ImeBridge を実装**

`core/CefUnity.Viewer/ImeBridge.cs`:

```csharp
namespace CefUnity.Viewer
{
    /// <summary>ImeBridge の出力先 (テストでは fake、実行時は Browser アダプタ)。</summary>
    public interface IImeSink
    {
        void SetComposition(string text, uint cursorPosition);
        void CommitText(string text);
        void SendCharacter(char character);
        void FinishComposition();
        void CancelComposition();
    }

    /// <summary>
    ///     SDL TEXTEDITING/TEXTINPUT ⇔ CEF IME API の状態機械 (spec §ImeBridge)。
    ///     変換中 (Composing) の TEXTINPUT は確定、非変換の TEXTINPUT は素の文字入力。
    /// </summary>
    public sealed class ImeBridge
    {
        private readonly IImeSink _sink;

        public ImeBridge(IImeSink sink)
        {
            _sink = sink;
        }

        public bool Composing { get; private set; }

        public void OnTextEditing(string text, int cursorStart)
        {
            if (text.Length > 0)
            {
                Composing = true;
                _sink.SetComposition(text, (uint)cursorStart);
            }
            else if (Composing)
            {
                Composing = false;
                _sink.CancelComposition();
            }
        }

        public void OnTextInput(string text)
        {
            if (Composing)
            {
                Composing = false;
                _sink.CommitText(text);
            }
            else
            {
                foreach (var character in text) _sink.SendCharacter(character);
            }
        }

        public void OnFocusLost()
        {
            if (!Composing) return;
            Composing = false;
            _sink.FinishComposition();
        }
    }
}
```

- [ ] **Step 4: テスト成功を確認**

Run: `dotnet test core/CefUnity.Tests --filter ImeBridgeTests 2>&1 | tail -3`
Expected: PASS

- [ ] **Step 5: ViewerWindow に SDL IME 配線**

`ViewerWindow.cs` に追加。まず Browser アダプタ (ファイル末尾、同 namespace 内の別クラスとして):

```csharp
    /// <summary>IImeSink → Browser の橋 (実行時配線)。</summary>
    internal sealed class BrowserImeSink : IImeSink
    {
        private readonly CefUnity.Interop.Browser _browser;

        public BrowserImeSink(CefUnity.Interop.Browser browser)
        {
            _browser = browser;
        }

        public void SetComposition(string text, uint cursorPosition) => _browser.ImeSetComposition(text, cursorPosition, cursorPosition);
        public void CommitText(string text) => _browser.ImeCommitText(text);
        public void SendCharacter(char character) => _browser.SendCharEvent(character);
        public void FinishComposition() => _browser.ImeFinishComposingText();
        public void CancelComposition() => _browser.ImeCancelComposition();
    }
```

ViewerWindow フィールド追加:

```csharp
        private ImeBridge? _imeBridge;
        private static ViewerWindow? _eventWatchInstance; // SDL コールバックは static のため
        private int _caretX = -1;
        private int _caretY = -1;
```

`OnLoad()` 末尾に追加:

```csharp
            _imeBridge = new ImeBridge(new BrowserImeSink(_frameSource.Browser));
            _eventWatchInstance = this;
            unsafe { _sdl.StartTextInput(); _sdl.AddEventWatch(new PfnEventFilter(ImeEventWatch), null); }
```

static コールバックとインスタンスハンドラ (Silk.NET の `TextEditingEvent.Text` / `TextInputEvent.Text` は fixed byte バッファ — UTF-8 を NUL 終端まで読む):

```csharp
        private static unsafe int ImeEventWatch(void* userData, Event* sdlEvent)
        {
            var instance = _eventWatchInstance;
            if (instance == null) return 1;
            switch ((EventType)sdlEvent->Type)
            {
                case EventType.Textediting:
                    instance._imeBridge?.OnTextEditing(ReadFixedUtf8(sdlEvent->Edit.Text, 32), sdlEvent->Edit.Start);
                    break;
                case EventType.Textinput:
                    instance._imeBridge?.OnTextInput(ReadFixedUtf8(sdlEvent->Text.Text, 32));
                    break;
            }
            return 1;
        }

        private static unsafe string ReadFixedUtf8(byte* bytes, int capacity)
        {
            var length = 0;
            while (length < capacity && bytes[length] != 0) length++;
            return System.Text.Encoding.UTF8.GetString(bytes, length);
        }
```

`OnRender` 末尾にキャレット追従を追加 (SDL_SetTextInputRect は窓ローカル論理座標):

```csharp
            _frameSource.Browser.GetImeCaret(out var caretX, out var caretY, out var caretWidth, out var caretHeight);
            if ((caretX != _caretX || caretY != _caretY) && caretWidth >= 0)
            {
                _caretX = caretX;
                _caretY = caretY;
                var rectangle = new Silk.NET.Maths.Rectangle<int>(caretX, caretY, Math.Max(caretWidth, 1), Math.Max(caretHeight, 16));
                unsafe { _sdl.SetTextInputRect(&rectangle); }
            }
```

(SetTextInputRect の引数型はコンパイルエラーで判明する — `Silk.NET.Maths.Rectangle<int>` でなければ Silk.NET.SDL 側の SDL_Rect 相当struct (int X,Y,W,H の 4 フィールド順序は同一レイアウト) に読み替える)
`Dispose()` に `_eventWatchInstance = null;` を追加。

- [ ] **Step 6: 手動確認 — 日本語入力の通し**

Run: `dotnet run --project core/CefUnity.Viewer -- --url https://www.google.com`
手順: 検索欄クリック → 日本語 IME で「かんじ」→ 変換 → Enter 確定。
Expected: 変換中の下線テキストがページ内に出る (ImeSetComposition)、確定で「漢字」が入る (ImeCommitText)、IME 候補窓がキャレット近くに出る (SetTextInputRect)。英数直接タイプも文字が入る (SendCharEvent — Task 5 で入らなかった部分がここで成立)。
NG の場合: spike (Task 2) の textEvents カウントを再確認 → TEXTEDITING が来ていなければ SDL ヒント `SDL_HINT_IME_SHOW_UI` / `SDL_HINT_IME_SUPPORT_EXTENDED_TEXT` を `_sdl.SetHint(...)` で有効化して再試験。

- [ ] **Step 7: Commit**

```bash
git add core/CefUnity.Viewer core/CefUnity.Tests
git commit -m "feat: Viewer IME (SDL TEXTEDITING/TEXTINPUT ⇔ CEF IME API + 候補窓追従)"
```

---

### Task 10: リサイズ + 終了処理 + README + 総合検証

**Files:**
- Modify: `core/CefUnity.Viewer/ViewerWindow.cs`
- Create: `core/CefUnity.Viewer/README.md`

**Interfaces:**
- Consumes: `CefFrameSource.Resize` (Task 3)、`ImeBridge.OnFocusLost` (Task 9)

- [ ] **Step 1: リサイズとフォーカス配線を追加**

`ViewerWindow.cs` のコンストラクタ (`_window.Load += OnLoad;` の並び) に追加:

```csharp
            _window.Resize += OnWindowResize;
            _window.FocusChanged += OnFocusChanged;
```

ハンドラ:

```csharp
        private void OnWindowResize(Silk.NET.Maths.Vector2D<int> newSize)
        {
            if (newSize.X <= 0 || newSize.Y <= 0) return;
            _frameSource.Resize(newSize.X, newSize.Y);
            // drawableSize は Present がテクスチャサイズ追従で更新する。
            // 新サイズのテクスチャが届くまでは旧フレームがレイヤーにスケール表示される (spec)
        }

        private void OnFocusChanged(bool focused)
        {
            if (!focused) _imeBridge?.OnFocusLost();
        }
```

- [ ] **Step 2: 手動確認 — リサイズ**

Run: `dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org`
Expected: 窓ドラッグでリサイズ → 一瞬旧フレームが伸縮表示された後、新サイズでレイアウトし直したページが映る (10 秒待ちの再描画停滞が出ないこと — server 側 invalidate の確認)。

- [ ] **Step 3: README を書く**

`core/CefUnity.Viewer/README.md`:

```markdown
# CefUnity.Viewer

Unity 外で CEF を表示+操作する mac 単体ブラウザ。スクロールカクツキの
Unity 固有性切り分けが主目的 (spec: docs/superpowers/specs/2026-07-25-silknet-viewer-design.md)。

## 実行

    dotnet run --project core/CefUnity.Viewer -- [--url <url>] [--size 1280x720]
        [--scroll-mode raw|smoother|resampler] [--record]
        [--replay <events-csv>] [--statistics <output-csv>] [--analyze <statistics-csv>]
    dotnet run --project core/CefUnity.Viewer -- spike   # SDL/Metal/NSEvent/IME 疎通確認

## 実行時ショートカット

| キー | 動作 |
|---|---|
| F1 / F2 / F3 | スクロールモード切替 raw / smoother / resampler (タイトルに表示) |
| F5 | 生イベント録画トグル → $TMPDIR/cef_scroll_events.csv |

## 切り分けの実験プロトコル

1. Unity (または Viewer) で `--record` 相当の録画を取る
2. `--replay <csv> --statistics <out.csv>` で同一入力を Viewer に再生 (再生中は窓をアクティブに保つ — 非アクティブだと CEF paint が凍結)
3. `--analyze <out.csv>` で粗さ指標を計算し、ホスト間で比較する
   (比較は必ず本コマンド同士で行う — 過去のアドホック集計値との直接比較はしない)

## トラブルシューティング

- サーバープロセス残留 (次回起動が永久ハング): `pkill -f cef-unity-server`
- 起動ハング (キャッシュ破損): `$TMPDIR` 配下の cef_unity_cache を削除
- スクロール resampler モードが効かない: 起動ログの `native scroll source:` を確認
```

- [ ] **Step 4: 全テスト + 総合手動検証**

Run: `dotnet test core/CefUnity.Tests 2>&1 | tail -3` → PASS (全緑、既存テスト含む)
総合手動チェックリスト (`dotnet run --project core/CefUnity.Viewer -- --url https://ja.wikipedia.org/wiki/%E7%8C%AB`):
- [ ] ページが映る・タイトルが `[Resampler]`
- [ ] クリック/ダブルクリック/ホバー
- [ ] トラックパッドスクロールが滑らか、F1 で明確に粗くなる
- [ ] 矢印/PageDown/Tab キー
- [ ] 日本語入力 (変換中下線 + 候補窓位置 + 確定)
- [ ] リサイズ追従
- [ ] 窓を閉じてサーバー残留なし (`ps aux | grep cef-unity-server | grep -v grep` が空)
- [ ] `--record` → `--replay` → `--statistics` → `--analyze` の通し

- [ ] **Step 5: Commit**

```bash
git add core/CefUnity.Viewer
git commit -m "feat: Viewer リサイズ/フォーカス処理と README"
```

---

## 実装後 (別途)

- 実験プロトコルの実施 (Unity 録画 → Viewer リプレイ → roughness 比較) は実装完了後の**検証タスク**として別途行う — 結果が本プロジェクトの結論 (再現=Core/CEF 側、非再現=Unity 側)
- superpowers:finishing-a-development-branch でブランチ統合 (main 直コミットでなくブランチ作業の場合)
