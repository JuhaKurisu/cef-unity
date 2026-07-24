# 逆転設計: 純 .NET コア + Unity は DLL 消費 — 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** スクロール/音声/ペーサ等のロジック単体テスト(60 件)と実機 CEF ハーネスを、Unity を起動せず `dotnet test` / `dotnet run` で回せるようにする。純 .NET の `CefUnity.Core` を真実源とし、Unity はビルド済み Core.dll を消費する1アダプタ・ホストへ降格する。

**Architecture:** Ports & Adapters(ヘキサゴナル)。Core(netstandard2.1)に Interop(P/Invoke 一本化)+ 純ロジック + ScrollReplay ロジックを集約。Unity・Harness・Tests が Core を消費する。唯一の差込口は既存 `IScrollEventSource`(時間 `dt` は既に引数注入)。新設抽象はゼロ。

**Tech Stack:** .NET 11 SDK(`dotnet 11.0.100-preview`)、netstandard2.1(Core)、net10.0(Harness/Tests)、NUnit、Unity 6000.3.8f1、Rust ネイティブ dylib(`cef_unity_rust`)、git LFS。

## Global Constraints

- Core は **netstandard2.1** 単一ターゲット、`LangVersion latest`、`AllowUnsafeBlocks true`、`Nullable enable`。Harness/Tests は **net10.0**。
- Core は **UnityEngine を一切参照しない**(独立 .csproj ゆえ構造的に不可能)。
- 名前空間は既存を維持: `CefUnity`(NativeMethods)/ `CefUnity.Interop`(Browser・CefRuntime・enum・CefKeyCode)/ `CefUnity.Runtime`(スクロール・音声リング・ScrollReplay)。**型を二重定義しない**(同一型が Core.dll と Unity asmdef の両方に存在してはならない)。
- ネイティブ P/Invoke は `[DllImport("cef_unity_rust", …Cdecl, ExactSpelling…)]` を維持(`[LibraryImport]` 化しない)。
- コミット署名: **Co-Authored-By を付けない**。author は `Juha <sakastudio@moores.tech>`。
- 移設は **`git mv`(コピーではなく移動)**。Option B は真実源が Assets 外に1つだけ。移動途中 Unity は「一時的にコンパイル不能」になるが、`dotnet` 側の成果物は各フェーズで独立に検証可能。Unity の非回帰は Phase 5 で最後にまとめて復旧・検証する。
- 各コミットは `--author="Juha <sakastudio@moores.tech>"` を付す。

---

## ファイル構成(この計画で作成/変更するもの)

**新規作成(Assets 外, 真実源):**
- `core/CefUnity.sln` — ソリューション
- `core/CefUnity.Core/CefUnity.Core.csproj` — netstandard2.1 ライブラリ
- `core/CefUnity.Core/Interop/CefUnity.cs`, `Interop/NativeMethods.g.cs` — Interop 一本化(移動先)
- `core/CefUnity.Core/Scroll/ScrollSmoother.cs`, `Scroll/CefZeroFramePacer.cs` — 移動先
- `core/CefUnity.Core/Audio/CefAudioRing.cs` — 移動先
- `core/CefUnity.Core/ScrollInput/*.cs` — 移動先(4 ファイル)
- `core/CefUnity.Core/Replay/ScrollReplayRunner.cs` — ScrollReplay ロジック抽出(新規)
- `core/CefUnity.Harness/CefUnity.Harness.csproj`, `Program.cs`, `Directory.Build.targets` — ヘッドレスホスト
- `core/CefUnity.Tests/CefUnity.Tests.csproj` — テストプロジェクト
- `core/CefUnity.Tests/*.cs` — 60 テスト(移動先)
- `core/build-core.sh` — Core.dll ビルド&配置スクリプト

**変更(Unity 側, Phase 5):**
- `cef-unity-unityproject/Assets/CefUnity/Editor/CefBuildPostProcessor.cs` — パス文字列2箇所
- `cef-unity-unityproject/Assets/CefUnity/Editor/ScrollReplay.cs` — Core 呼び出しの薄ラッパ化
- `cef-unity-unityproject/Assets/CefUnity/Runtime/Script.asmdef` — Core.dll precompiled 参照
- 移動: `Assets/CefUnity/Interop/Plugins/**` → `Assets/CefUnity/Plugins/**`
- 配置: `Assets/CefUnity/Plugins/CefUnity.Core.dll`(+ `.meta`, LFS)
- 削除: `Assets/CefUnity/Interop/`(マネージド `.cs` + asmdef), `Assets/CefUnity/Runtime/{ScrollSmoother,CefZeroFramePacer,CefAudioRing}.cs`, `Assets/CefUnity/Runtime/ScrollInput/`, `Assets/CefUnity/Runtime.Tests/`

**削除(旧 standalone, Phase 1/3 で吸収):**
- `cef-unity-csharp/`(Interop + Sandbox → Core + Harness へ)

**変更(CI, Phase 6):**
- `.github/workflows/rust-build.yml`

---

## Phase 0 — Core ソリューションの足場

### Task 0.1: Core ソリューションと空ライブラリの作成

**Files:**
- Create: `core/CefUnity.Core/CefUnity.Core.csproj`
- Create: `core/CefUnity.sln`

**Interfaces:**
- Produces: `CefUnity.Core` アセンブリ(netstandard2.1)。後続タスクがこの csproj に `<Compile>` されるソースを追加していく。

- [ ] **Step 1: Core csproj を作成**

`core/CefUnity.Core/CefUnity.Core.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>netstandard2.1</TargetFramework>
    <LangVersion>latest</LangVersion>
    <Nullable>enable</Nullable>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <AssemblyName>CefUnity.Core</AssemblyName>
    <RootNamespace>CefUnity</RootNamespace>
  </PropertyGroup>
</Project>
```

- [ ] **Step 2: ソリューションを作成し Core を追加**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity/core
dotnet new sln -n CefUnity
dotnet sln add CefUnity.Core/CefUnity.Core.csproj
```
Expected: `Project ... added to the solution.`

- [ ] **Step 3: 空ビルドが通ることを確認**

Run: `dotnet build core/CefUnity.Core/CefUnity.Core.csproj`
Expected: `Build succeeded.`(警告 CS2008「コンパイルするソースがない」相当が出る場合はダミーが要るが、SDK スタイルは空でも成功する。空成功しない環境なら次タスクで解消するので続行可)

- [ ] **Step 4: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add core/CefUnity.sln core/CefUnity.Core/CefUnity.Core.csproj
git commit -m "feat(core): CefUnity.Core ソリューション足場 (netstandard2.1)" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 1 — Interop を Core に一本化 + Harness スモーク

このフェーズで「Interop を DLL 化しても実 CEF が回る」ことを端から端まで実証する。

### Task 1.1: Interop を Core へ移動し `#if` を実行時 OS 判定へ置換

**Files:**
- Move: `cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs` → `core/CefUnity.Core/Interop/CefUnity.cs`
- Move: `cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs` → `core/CefUnity.Core/Interop/NativeMethods.g.cs`
- Delete: `cef-unity-csharp/Interop/CefUnity.cs`, `cef-unity-csharp/Interop/NativeMethods.g.cs`(重複を廃止)
- Modify: `core/CefUnity.Core/Interop/CefUnity.cs`(`IsAcceleratedConnected()` の `#if` 置換)

**Interfaces:**
- Produces: `CefUnity.Interop.Browser`, `CefUnity.Interop.CefRuntime`, `CefUnity.NativeMethods`, `CefUnity.Interop.CefKeyCode`/`CefKeyCodes`/各 enum。名前空間は Unity 側の既存値を維持。
- Consumes: ネイティブ `cef_unity_rust`(実行時解決)。

- [ ] **Step 1: Interop ソースを Core へ移動**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
mkdir -p core/CefUnity.Core/Interop
git mv cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs core/CefUnity.Core/Interop/CefUnity.cs
git mv cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs core/CefUnity.Core/Interop/NativeMethods.g.cs
git rm cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.cs.meta cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs.meta
git rm cef-unity-csharp/Interop/CefUnity.cs cef-unity-csharp/Interop/NativeMethods.g.cs
```
Expected: 移動/削除が stage される。

- [ ] **Step 2: `IsAcceleratedConnected()` の Unity プリプロセッサ分岐を実行時 OS 判定へ置換**

`core/CefUnity.Core/Interop/CefUnity.cs` の該当メソッド(現 538–547 行付近)を次に置換:
```csharp
public static bool IsAcceleratedConnected()
{
    if (System.Runtime.InteropServices.RuntimeInformation.IsOSPlatform(System.Runtime.InteropServices.OSPlatform.OSX))
        return IsIOSurfaceConnected();
    if (System.Runtime.InteropServices.RuntimeInformation.IsOSPlatform(System.Runtime.InteropServices.OSPlatform.Windows))
        return IsD3D11Connected() || IsD3D12Connected();
    return false;
}
```
(`#if UNITY_STANDALONE_OSX … #endif` ブロックを丸ごと上記へ差し替える。他に `#if UNITY_*` は Interop に存在しない。)

- [ ] **Step 3: Interop を Core csproj に含める(SDK glob なので明示不要を確認)**

SDK スタイルは既定で `**/*.cs` を含む。追加設定は不要。

- [ ] **Step 4: Core ビルドで Interop がコンパイルされることを確認**

Run: `dotnet build core/CefUnity.Core/CefUnity.Core.csproj -v quiet`
Expected: `Build succeeded.` 0 Error。(`ReadOnlySpan<byte>` は netstandard2.1 で解決。エラー時は不足 API を確認)

- [ ] **Step 5: コミット**

```bash
git add -A core/CefUnity.Core cef-unity-unityproject/Assets/CefUnity/Interop cef-unity-csharp/Interop
git commit -m "feat(core): Interop を Core へ一本化 + #if を RuntimeInformation へ置換" --author="Juha <sakastudio@moores.tech>"
```

### Task 1.2: Harness(旧 Sandbox)を作成し実 CEF スモーク

**Files:**
- Create: `core/CefUnity.Harness/CefUnity.Harness.csproj`
- Create: `core/CefUnity.Harness/Program.cs`
- Create: `core/CefUnity.Harness/Directory.Build.targets`(CEF framework コピー、旧 `cef-unity-csharp/Directory.Build.targets` 相当)
- Delete(後続で吸収): `cef-unity-csharp/Sandbox/*`, `cef-unity-csharp/Directory.Build.targets`, `cef-unity-csharp/cef-unity-csharp.slnx` は Phase 3 完了後に削除(Task 3.3)

**Interfaces:**
- Consumes: `CefUnity.Interop.CefRuntime.Init/Pump/Shutdown`, `CefUnity.Interop.Browser`(`TryGetBuffer`)。

- [ ] **Step 1: Harness csproj を作成(Core 参照 + dylib コピー)**

`core/CefUnity.Harness/CefUnity.Harness.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net10.0</TargetFramework>
    <ImplicitUsings>enable</ImplicitUsings>
    <Nullable>enable</Nullable>
    <RustProjectDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust'))</RustProjectDir>
    <RustTargetDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust\target\debug'))</RustTargetDir>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="..\CefUnity.Core\CefUnity.Core.csproj" />
  </ItemGroup>
  <ItemGroup>
    <None Include="$(RustTargetDir)/libcef_unity_rust.dylib"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.dylib" />
  </ItemGroup>
  <!-- cef-unity-server.app bundle を出力先に用意(旧 Sandbox と同じ) -->
  <Target Name="CopyServerApp" AfterTargets="Build">
    <Exec Command="bash '$(RustProjectDir)/build-server-sandbox.sh' '$(OutputPath)'" />
  </Target>
</Project>
```

- [ ] **Step 2: CEF framework コピー targets を作成**

`core/CefUnity.Harness/Directory.Build.targets`(旧 `cef-unity-csharp/Interop/CefUnity.targets` の CopyCefFramework 相当をそのまま移植):
```xml
<Project>
  <PropertyGroup>
    <_CefRustTargetDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust\target\debug'))</_CefRustTargetDir>
    <_CefFrameworkDest>$(OutputPath)Chromium Embedded Framework.framework</_CefFrameworkDest>
  </PropertyGroup>
  <Target Name="CopyCefFramework" AfterTargets="Build" Condition="!Exists('$(_CefFrameworkDest)')">
    <Exec Condition="$([MSBuild]::IsOSPlatform('OSX')) Or $([MSBuild]::IsOSPlatform('Linux'))"
          Command="rsync -a `ls -dt &quot;$(_CefRustTargetDir)&quot;/build/cef-dll-sys-*/out/cef_macos_aarch64 | head -1`/&quot;Chromium Embedded Framework.framework/&quot; &quot;$(_CefFrameworkDest)/&quot;" />
  </Target>
</Project>
```

- [ ] **Step 3: Program.cs を作成(旧 Sandbox の擬似ループ + 引数ディスパッチ)**

`core/CefUnity.Harness/Program.cs`:
```csharp
using CefUnity.Interop;

// サブコマンド: (なし)=スモーク, replay=Phase 4 で追加
var cmd = args.Length > 0 ? args[0] : "smoke";
if (cmd == "smoke")
{
    CefRuntime.Init();
    using (var browser = new Browser(1280, 720, "https://example.com"))
    {
        var frames = 0;
        for (var i = 0; i < 100; i++)
        {
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
Console.Error.WriteLine($"unknown command: {cmd}");
return 2;
```
(注: 旧 Sandbox は `using Interop;` だったが、Core は名前空間 `CefUnity.Interop` を使うため `using CefUnity.Interop;`。`Browser`/`CefRuntime`/`TryGetBuffer` のシグネチャは移動前と同一。)

- [ ] **Step 4: ソリューションに追加**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity/core
dotnet sln add CefUnity.Harness/CefUnity.Harness.csproj
```

- [ ] **Step 5: 実 CEF スモークを実行(要 Rust ビルド済み dylib)**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
dotnet run --project core/CefUnity.Harness -c Debug -- smoke
```
Expected: `Frame #1: 1280x720 …` が数行、最後に `SMOKE_OK frames=N`(N>0)、終了コード 0。
(dylib 未ビルド時は `cd cef-unity-rust && cargo build` を先に実施。CEF 起動が singleton ロックでハングしたら `pkill -f cef-unity-server` 後に再実行。)

- [ ] **Step 6: コミット**

```bash
git add core/CefUnity.sln core/CefUnity.Harness
git commit -m "feat(harness): 旧 Sandbox を Harness へ移植し Core.Interop 経由の実 CEF スモークを確認" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 2 — 純ロジッククラスを Core へ移動

### Task 2.1: スクロール/音声/ペーサの純クラスを移動

**Files:**
- Move: `Assets/CefUnity/Runtime/ScrollSmoother.cs` → `core/CefUnity.Core/Scroll/ScrollSmoother.cs`
- Move: `Assets/CefUnity/Runtime/CefZeroFramePacer.cs` → `core/CefUnity.Core/Scroll/CefZeroFramePacer.cs`
- Move: `Assets/CefUnity/Runtime/CefAudioRing.cs` → `core/CefUnity.Core/Audio/CefAudioRing.cs`
- Move: `Assets/CefUnity/Runtime/ScrollInput/{ScrollInputEvent,ScrollResampler,ScrollInputPipeline,MacNativeScrollSource}.cs` → `core/CefUnity.Core/ScrollInput/`

**Interfaces:**
- Produces: `CefUnity.Runtime.ScrollSmoother`(`Tick(float dt, float tau, out int dx, out int dy)`, `AddInput`, `Reset`, `IsActive`)、`CefUnity.Runtime.CefZeroFramePacer`、`CefUnity.Runtime.CefAudioRing`、`CefUnity.Runtime.ScrollResampler`(`Predictive` プロパティ)、`CefUnity.Runtime.ScrollInputPipeline`、`CefUnity.Runtime.IScrollEventSource`、`CefUnity.Runtime.ScrollInputEvent`、`CefUnity.Runtime.MacNativeScrollSource`。名前空間 `CefUnity.Runtime` を維持。

- [ ] **Step 1: 純クラスを Core へ移動(.meta も削除)**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
mkdir -p core/CefUnity.Core/Scroll core/CefUnity.Core/Audio core/CefUnity.Core/ScrollInput
R=cef-unity-unityproject/Assets/CefUnity/Runtime
git mv $R/ScrollSmoother.cs      core/CefUnity.Core/Scroll/ScrollSmoother.cs
git mv $R/CefZeroFramePacer.cs   core/CefUnity.Core/Scroll/CefZeroFramePacer.cs
git mv $R/CefAudioRing.cs        core/CefUnity.Core/Audio/CefAudioRing.cs
git mv $R/ScrollInput/ScrollInputEvent.cs     core/CefUnity.Core/ScrollInput/ScrollInputEvent.cs
git mv $R/ScrollInput/ScrollResampler.cs      core/CefUnity.Core/ScrollInput/ScrollResampler.cs
git mv $R/ScrollInput/ScrollInputPipeline.cs  core/CefUnity.Core/ScrollInput/ScrollInputPipeline.cs
git mv $R/ScrollInput/MacNativeScrollSource.cs core/CefUnity.Core/ScrollInput/MacNativeScrollSource.cs
git rm $R/ScrollSmoother.cs.meta $R/CefZeroFramePacer.cs.meta $R/CefAudioRing.cs.meta
git rm -r $R/ScrollInput
```
(`ScrollInput.meta`(フォルダ meta)も `git rm` 対象になる。残ればあわせて削除。)

- [ ] **Step 2: Core ビルドで全ロジックがコンパイルされることを確認**

Run: `dotnet build core/CefUnity.Core/CefUnity.Core.csproj -v quiet`
Expected: `Build succeeded.` 0 Error。

- [ ] **Step 3: コミット**

```bash
git add -A core/CefUnity.Core cef-unity-unityproject/Assets/CefUnity/Runtime
git commit -m "feat(core): スクロール/ペーサ/音声リングの純ロジックを Core へ移動" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 3 — Unity 無しの単体テスト(主目的)

### Task 3.1: Tests プロジェクトを作成しテストを移動

**Files:**
- Create: `core/CefUnity.Tests/CefUnity.Tests.csproj`
- Move: `Assets/CefUnity/Runtime.Tests/{CefAudioRingTests,CefZeroFramePacerTests,ScrollInputPipelineTests,ScrollResamplerTests,ScrollSmootherTests}.cs` → `core/CefUnity.Tests/`
- Move + Modify: `Assets/CefUnity/Runtime.Tests/ScrollDroughtRecordingTests.cs` → `core/CefUnity.Tests/`(`Application.dataPath` 依存を除去)

**Interfaces:**
- Consumes: `CefUnity.Runtime.*`(Core.dll)。

- [ ] **Step 1: Tests csproj を作成**

`core/CefUnity.Tests/CefUnity.Tests.csproj`:
```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net10.0</TargetFramework>
    <LangVersion>latest</LangVersion>
    <Nullable>enable</Nullable>
    <IsPackable>false</IsPackable>
  </PropertyGroup>
  <ItemGroup>
    <PackageReference Include="Microsoft.NET.Test.Sdk" Version="17.11.1" />
    <PackageReference Include="NUnit" Version="4.2.2" />
    <PackageReference Include="NUnit3TestAdapter" Version="4.6.0" />
  </ItemGroup>
  <ItemGroup>
    <ProjectReference Include="..\CefUnity.Core\CefUnity.Core.csproj" />
  </ItemGroup>
</Project>
```

- [ ] **Step 2: Unity 依存のない 5 テストを移動**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
T=cef-unity-unityproject/Assets/CefUnity/Runtime.Tests
git mv $T/CefAudioRingTests.cs        core/CefUnity.Tests/CefAudioRingTests.cs
git mv $T/CefZeroFramePacerTests.cs   core/CefUnity.Tests/CefZeroFramePacerTests.cs
git mv $T/ScrollInputPipelineTests.cs core/CefUnity.Tests/ScrollInputPipelineTests.cs
git mv $T/ScrollResamplerTests.cs     core/CefUnity.Tests/ScrollResamplerTests.cs
git mv $T/ScrollSmootherTests.cs      core/CefUnity.Tests/ScrollSmootherTests.cs
git rm $T/CefAudioRingTests.cs.meta $T/CefZeroFramePacerTests.cs.meta \
       $T/ScrollInputPipelineTests.cs.meta $T/ScrollResamplerTests.cs.meta $T/ScrollSmootherTests.cs.meta
```

- [ ] **Step 3: ソリューションに Tests を追加しビルド**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity/core
dotnet sln add CefUnity.Tests/CefUnity.Tests.csproj
dotnet build CefUnity.Tests/CefUnity.Tests.csproj -v quiet
```
Expected: `Build succeeded.`(`using CefUnity.Runtime;` / `using NUnit.Framework;` は解決。もし `using Interop;` を含むテストがあれば `using CefUnity.Interop;` に修正。移動した 5 件は `using CefUnity.Runtime` のみ)

- [ ] **Step 4: 移動した 5 テストを実行(緑を確認)**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet test core/CefUnity.Tests -v quiet`
Expected: `Passed!  - Failed: 0` で 55 前後(ScrollDroughtRecording を除く全 `[Test]`)。

- [ ] **Step 5: コミット**

```bash
git add -A core/CefUnity.Tests core/CefUnity.sln cef-unity-unityproject/Assets/CefUnity/Runtime.Tests
git commit -m "test(core): Unity 非依存の 5 テスト群を dotnet test へ移設" --author="Juha <sakastudio@moores.tech>"
```

### Task 3.2: ScrollDroughtRecordingTests の `Application.dataPath` 依存を除去

**Files:**
- Move + Modify: `Assets/CefUnity/Runtime.Tests/ScrollDroughtRecordingTests.cs` → `core/CefUnity.Tests/ScrollDroughtRecordingTests.cs`
- Create: `core/CefUnity.Tests/fixtures/cef_scroll_events_nozerowait.csv`(`test-results/scroll-drought-2026-07-23/` から複製)+ csproj の `<None Include="fixtures/**" …>`

**Interfaces:**
- Consumes: `CefUnity.Runtime.ScrollResampler`(`AddEvent(in ScrollInputEvent)`, `Tick(double, out int, out int)`, `Predictive`)。録画フィクスチャ `test-results/scroll-drought-2026-07-23/cef_scroll_events_nozerowait.csv`(リポジトリ同梱)。

- [ ] **Step 1: テストを移動**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
T=cef-unity-unityproject/Assets/CefUnity/Runtime.Tests
git mv $T/ScrollDroughtRecordingTests.cs core/CefUnity.Tests/ScrollDroughtRecordingTests.cs
git rm $T/ScrollDroughtRecordingTests.cs.meta
```

- [ ] **Step 2: 録画フィクスチャを Tests プロジェクトへ複製し csproj に登録**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
mkdir -p core/CefUnity.Tests/fixtures
cp test-results/scroll-drought-2026-07-23/cef_scroll_events_nozerowait.csv \
   core/CefUnity.Tests/fixtures/cef_scroll_events_nozerowait.csv
```
`core/CefUnity.Tests/CefUnity.Tests.csproj` に追加:
```xml
  <ItemGroup>
    <None Include="fixtures/**" CopyToOutputDirectory="PreserveNewest" />
  </ItemGroup>
```

- [ ] **Step 3: `Application.dataPath` 依存を除去(fixtures から解決)**

`core/CefUnity.Tests/ScrollDroughtRecordingTests.cs` の `using UnityEngine;` を削除し、パス生成を差し替える:
```csharp
// 旧: Path.Combine(Application.dataPath, "..", "..",
//         "test-results/scroll-drought-2026-07-23/cef_scroll_events_nozerowait.csv")
// 新: 実行アセンブリ隣の fixtures から解決(見つからなければ Assert.Ignore は従来どおり)
var path = System.IO.Path.Combine(
    AppContext.BaseDirectory, "fixtures", "cef_scroll_events_nozerowait.csv");
```
(名前空間 `CefUnity.Runtime.Tests` はそのまま。`Assert.Ignore` フォールバックは維持。)

- [ ] **Step 4: 全 60 テストが緑になることを確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet test core/CefUnity.Tests -v quiet`
Expected: `Passed!  - Failed: 0`、合計 60。

- [ ] **Step 5: 空になった Runtime.Tests ディレクトリを削除**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
git rm -f cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/Runtime.Tests.asmdef \
         cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/Runtime.Tests.asmdef.meta 2>/dev/null || true
# 残る .meta / 空ディレクトリを掃除
git status --porcelain cef-unity-unityproject/Assets/CefUnity/Runtime.Tests
```
Expected: Runtime.Tests 配下が空(asmdef 削除で Unity Test Runner 版は廃止)。

- [ ] **Step 6: コミット**

```bash
git add -A core/CefUnity.Tests cef-unity-unityproject/Assets/CefUnity/Runtime.Tests
git commit -m "test(core): ScrollDrought を fixtures 化し全 60 テストを dotnet test で緑化・Runtime.Tests asmdef 廃止" --author="Juha <sakastudio@moores.tech>"
```

### Task 3.3: 旧 `cef-unity-csharp/` を削除(Core へ吸収完了)

**Files:**
- Delete: `cef-unity-csharp/`(全体)

- [ ] **Step 1: 旧 standalone プロジェクトを削除**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
git rm -r cef-unity-csharp
```
Expected: Interop/Sandbox/slnx/targets 一式が削除 stage。

- [ ] **Step 2: Core ソリューションが独立して健全なことを再確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && dotnet build core/CefUnity.sln -v quiet && dotnet test core/CefUnity.Tests -v quiet`
Expected: `Build succeeded.` と `Passed! - Failed: 0`(60)。

- [ ] **Step 3: コミット**

```bash
git commit -am "chore: 旧 cef-unity-csharp を削除 (core/ へ吸収完了)" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 4 — ScrollReplay を Core へ + Harness リプレイ

### Task 4.1: ScrollReplay ロジックを Core の純ランナーへ抽出

既存 `Editor/ScrollReplay.cs` の `Run()`(S/E/T CSV → `ScrollResampler` 二系統リプレイ → live 実排出との忠実度測定)から、Unity I/O・`EditorApplication.Exit`・`Debug.Log` を剥がした純ロジックを Core へ移す。判定式・列フォーマットは既存と**完全同一**にする(新判定を発明しない)。

**Files:**
- Create: `core/CefUnity.Core/Replay/ScrollReplayRunner.cs`
- Test: `core/CefUnity.Tests/ScrollReplayRunnerTests.cs`

**Interfaces:**
- Produces: `CefUnity.Runtime.ScrollReplayRunner.Run(IEnumerable<string> csvLines) -> ScrollReplayResult`。`ScrollReplayResult { bool Ok; string? Error; int Events; int Ticks; int Mismatches; IReadOnlyList<string> OutLines; }`。
- Consumes: `CefUnity.Runtime.ScrollResampler`(`AddEvent(in ScrollInputEvent)`, `Tick(double now, out int dx, out int dy)`, `Predictive`)、`ScrollInputEvent`(`Timestamp/DxPx/DyPx/Phase/Precise`)、`ScrollPhase`。

- [ ] **Step 1: 失敗するテストを書く**

`core/CefUnity.Tests/ScrollReplayRunnerTests.cs`:
```csharp
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Tests
{
    public class ScrollReplayRunnerTests
    {
        [Test]
        public void ValidCsv_ParsesEventsAndTicks()
        {
            // S=scale, E=timestamp,dx,dy,phase,precise[,over], T=now,liveDx,liveDy,mode
            var lines = new[]
            {
                "S,1",
                "E,0.000,0,-8,2,1",
                "E,0.016,0,-8,2,1",
                "T,0.016,0,-4,0",
                "T,0.032,0,-4,0",
            };
            var r = ScrollReplayRunner.Run(lines);
            Assert.That(r.Ok, Is.True, r.Error);
            Assert.That(r.Events, Is.EqualTo(2));
            Assert.That(r.Ticks, Is.EqualTo(2));
            Assert.That(r.OutLines.Count, Is.EqualTo(2));
        }

        [Test]
        public void NoTickLines_Fails()
        {
            var r = ScrollReplayRunner.Run(new[] { "S,1", "E,0.0,0,-8,2,1" });
            Assert.That(r.Ok, Is.False);
        }
    }
}
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollReplayRunnerTests -v quiet`
Expected: FAIL(`ScrollReplayRunner` / `ScrollReplayResult` 未定義でコンパイルエラー)。

- [ ] **Step 3: Core にランナーを実装(既存 Run() を忠実移植)**

`core/CefUnity.Core/Replay/ScrollReplayRunner.cs`:
```csharp
using System;
using System.Collections.Generic;
using System.Globalization;

namespace CefUnity.Runtime
{
    public sealed class ScrollReplayResult
    {
        public bool Ok;
        public string? Error;
        public int Events;
        public int Ticks;
        public int Mismatches;
        public IReadOnlyList<string> OutLines = Array.Empty<string>();
    }

    /// <summary>
    ///   cef_scroll_record の S/E/T CSV を ScrollResampler(interp + predictive)へ
    ///   オフラインリプレイし、録画時の live 実排出との忠実度(mismatches)を測る純ロジック。
    ///   Editor/Harness は本クラスを呼び、I/O・終了コード・ログのみ担当する。
    /// </summary>
    public static class ScrollReplayRunner
    {
        public static ScrollReplayResult Run(IEnumerable<string> csvLines)
        {
            var interp = new ScrollResampler();
            var pred = new ScrollResampler { Predictive = true };
            var outLines = new List<string>();
            var scale = 1f;                       // S 行が無い旧録画は scale=1
            int events = 0, ticks = 0, mismatches = 0, lineNo = 0;
            foreach (var line in csvLines)
            {
                lineNo++;
                if (line.Length == 0) continue;
                try
                {
                    var c = line.Split(',');
                    if (c.Length >= 2 && c[0] == "S")
                    {
                        scale = float.Parse(c[1], CultureInfo.InvariantCulture);
                    }
                    else if (c.Length >= 6 && c[0] == "E")
                    {
                        if (c.Length >= 7 && c[6] != "1") continue;   // live 未転送は投入しない
                        var e = new ScrollInputEvent
                        {
                            Timestamp = double.Parse(c[1], CultureInfo.InvariantCulture),
                            DxPx = float.Parse(c[2], CultureInfo.InvariantCulture) * scale,
                            DyPx = float.Parse(c[3], CultureInfo.InvariantCulture) * scale,
                            Phase = (ScrollPhase)byte.Parse(c[4], CultureInfo.InvariantCulture),
                            Precise = c[5] == "1",
                        };
                        if (!e.Precise) continue;                     // precise のみ
                        interp.AddEvent(in e);
                        pred.AddEvent(in e);
                        events++;
                    }
                    else if (c.Length >= 5 && c[0] == "T")
                    {
                        var now = double.Parse(c[1], CultureInfo.InvariantCulture);
                        interp.Tick(now, out var idx, out var idy);
                        pred.Tick(now, out var pdx, out var pdy);
                        outLines.Add($"{c[1]},{c[2]},{c[3]},{idx},{idy},{pdx},{pdy}");
                        var wasPredictive = c[4] == "1";
                        var liveDx = int.Parse(c[2], CultureInfo.InvariantCulture);
                        var liveDy = int.Parse(c[3], CultureInfo.InvariantCulture);
                        if ((wasPredictive ? pdx : idx) != liveDx || (wasPredictive ? pdy : idy) != liveDy)
                            mismatches++;
                        ticks++;
                    }
                }
                catch (Exception ex)
                {
                    return new ScrollReplayResult { Ok = false, Error = $"parse error at line {lineNo}: \"{line}\" ({ex.Message})" };
                }
            }
            if (ticks == 0)
                return new ScrollReplayResult { Ok = false, Error = "no T lines (録画が空)" };
            return new ScrollReplayResult
            {
                Ok = true, Events = events, Ticks = ticks, Mismatches = mismatches, OutLines = outLines,
            };
        }
    }
}
```

- [ ] **Step 4: テストが通ることを確認**

Run: `dotnet test core/CefUnity.Tests --filter ScrollReplayRunnerTests -v quiet`
Expected: `Passed! - Failed: 0`(2 件)。

- [ ] **Step 5: コミット**

```bash
git add core/CefUnity.Core/Replay core/CefUnity.Tests/ScrollReplayRunnerTests.cs
git commit -m "feat(core): ScrollReplay の忠実度ロジックを ScrollReplayRunner へ抽出" --author="Juha <sakastudio@moores.tech>"
```

### Task 4.2: Harness に `replay` サブコマンドを追加

**Files:**
- Modify: `core/CefUnity.Harness/Program.cs`

**Interfaces:**
- Consumes: `CefUnity.Runtime.ScrollReplayRunner.Run(IEnumerable<string>)`(Task 4.1)。録画 CSV(`cef_scroll_record` 出力、S/E/T 形式)。

- [ ] **Step 1: replay 分岐を追加(Runner に行をそのまま渡す)**

`core/CefUnity.Harness/Program.cs` の `if (cmd == "smoke") { … }` の後に追加:
```csharp
if (cmd == "replay")
{
    if (args.Length < 2) { Console.Error.WriteLine("usage: replay <recording-csv>"); return 2; }
    var result = CefUnity.Runtime.ScrollReplayRunner.Run(File.ReadLines(args[1]));
    if (!result.Ok) { Console.Error.WriteLine($"REPLAY FAIL: {result.Error}"); return 1; }
    Console.WriteLine($"REPLAY ok events={result.Events} ticks={result.Ticks} mismatches={result.Mismatches}/{result.Ticks}");
    return 0;
}
```

- [ ] **Step 2: 同梱録画でリプレイが回ることを確認**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
dotnet run --project core/CefUnity.Harness -c Debug -- replay \
  test-results/scroll-drought-2026-07-23/cef_scroll_events_nozerowait.csv
```
Expected: `REPLAY ok events=… ticks=… mismatches=…/…`、終了コード 0。

- [ ] **Step 3: コミット**

```bash
git add core/CefUnity.Harness/Program.cs
git commit -m "feat(harness): 録画リプレイ自己検証を dotnet run -- replay で実行可能に" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 5 — Unity 側の再配線(Unity を復旧)

このフェーズ完了まで Unity プロジェクトはコンパイル不能(Phase 1 以降の移動による)。ここでまとめて復旧する。

### Task 5.1: ネイティブ Plugins を `Assets/CefUnity/Plugins/` へ移設(案B)

**Files:**
- Move: `Assets/CefUnity/Interop/Plugins/**` → `Assets/CefUnity/Plugins/**`
- Move: `Assets/CefUnity/Interop/.gitattributes` → `Assets/CefUnity/Plugins/.gitattributes`(パターン調整)
- Delete: `Assets/CefUnity/Interop/`(空になった後)

- [ ] **Step 1: ネイティブ Plugins を移動(.meta ごと・GUID 保持)**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
I=cef-unity-unityproject/Assets/CefUnity/Interop
P=cef-unity-unityproject/Assets/CefUnity/Plugins
mkdir -p $P
git mv $I/Plugins/osx-arm64 $P/osx-arm64
git mv $I/Plugins/win-x64   $P/win-x64
# フォルダ meta があれば移動
[ -f $I/Plugins.meta ] && git mv $I/Plugins.meta $P.meta || true
```

- [ ] **Step 2: LFS の .gitattributes を新パスへ**

`cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes` を作成(旧 Interop/.gitattributes と同内容):
```
Plugins/** filter=lfs diff=lfs merge=lfs -text
Plugins/**/*.meta -filter -diff -merge text
```
※ パターンは当該 .gitattributes からの相対。配置場所が `Plugins/` 直下なので、`osx-arm64/** …` 形に調整して確実に一致させる:
```
osx-arm64/** filter=lfs diff=lfs merge=lfs -text
win-x64/**   filter=lfs diff=lfs merge=lfs -text
**/*.meta -filter -diff -merge text
```
旧 `$I/.gitattributes` は `git rm`。

- [ ] **Step 3: LFS 追跡が新パスで有効か確認**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
git check-attr filter -- cef-unity-unityproject/Assets/CefUnity/Plugins/osx-arm64/libcef_unity_rust.dylib
```
Expected: `filter: lfs`。

- [ ] **Step 4: 空の Interop ディレクトリを削除**

Run:
```bash
cd /Users/juha/Documents/GitHub/cef-unity
git rm -f cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.Interop.asmdef \
         cef-unity-unityproject/Assets/CefUnity/Interop/CefUnity.Interop.asmdef.meta 2>/dev/null || true
git status --porcelain cef-unity-unityproject/Assets/CefUnity/Interop
```
Expected: Interop 配下が空(マネージド .cs は Phase 1 で移動済み、Plugins は移設済み、asmdef 削除)。

- [ ] **Step 5: コミット**

```bash
git add -A cef-unity-unityproject/Assets/CefUnity
git commit -m "refactor(unity): ネイティブ Plugins を Assets/CefUnity/Plugins へ移設・Interop asmdef 廃止 (案B)" --author="Juha <sakastudio@moores.tech>"
```

### Task 5.2: build-core.sh で Core.dll を生成し Unity Plugins へ配置(LFS)

**Files:**
- Create: `core/build-core.sh`
- Create(生成物): `Assets/CefUnity/Plugins/CefUnity.Core.dll`(+ `.meta`)
- Modify: `Assets/CefUnity/Plugins/.gitattributes`(dll を LFS 追跡)
- Modify: `core/CefUnity.Core/CefUnity.Core.csproj`(post-build コピー)

- [ ] **Step 1: build-core.sh を作成**

`core/build-core.sh`:
```bash
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
DEST="$HERE/../cef-unity-unityproject/Assets/CefUnity/Plugins"
dotnet build "$HERE/CefUnity.Core/CefUnity.Core.csproj" -c Release -v quiet
SRC="$HERE/CefUnity.Core/bin/Release/netstandard2.1/CefUnity.Core.dll"
cp "$SRC" "$DEST/CefUnity.Core.dll"
echo "copied CefUnity.Core.dll -> $DEST"
```
Run: `chmod +x core/build-core.sh`

- [ ] **Step 2: csproj に post-build コピーを追加(dotnet build 一発反映)**

`core/CefUnity.Core/CefUnity.Core.csproj` の `</Project>` 直前に追加:
```xml
  <Target Name="CopyToUnityPlugins" AfterTargets="Build" Condition="'$(Configuration)'=='Release'">
    <Copy SourceFiles="$(OutputPath)CefUnity.Core.dll"
          DestinationFolder="$(MSBuildThisFileDirectory)..\..\cef-unity-unityproject\Assets\CefUnity\Plugins" />
  </Target>
```

- [ ] **Step 3: Core.dll を生成・配置**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && ./core/build-core.sh`
Expected: `copied CefUnity.Core.dll -> …/Assets/CefUnity/Plugins`。`ls -la cef-unity-unityproject/Assets/CefUnity/Plugins/CefUnity.Core.dll` で存在確認。

- [ ] **Step 4: DLL を LFS 追跡に追加**

`Assets/CefUnity/Plugins/.gitattributes` に1行追加:
```
*.dll filter=lfs diff=lfs merge=lfs -text
```
Run: `git check-attr filter -- cef-unity-unityproject/Assets/CefUnity/Plugins/CefUnity.Core.dll`
Expected: `filter: lfs`。

- [ ] **Step 5: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add core/build-core.sh core/CefUnity.Core/CefUnity.Core.csproj \
        cef-unity-unityproject/Assets/CefUnity/Plugins/CefUnity.Core.dll \
        cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes
git commit -m "build(core): build-core.sh で Core.dll を生成し Unity Plugins へ LFS 配置" --author="Juha <sakastudio@moores.tech>"
```

### Task 5.3: Unity asmdef と Editor コードを Core.dll 参照へ再配線

**Files:**
- Modify: `Assets/CefUnity/Runtime/Script.asmdef`(Core.dll を precompiled 参照、autoReferenced 確認)
- Modify: `Assets/CefUnity/Editor/ScrollReplay.cs`(Core の `ScrollReplayRunner` を呼ぶ薄ラッパ化)
- Modify: `Assets/CefUnity/Editor/CefBuildPostProcessor.cs`(パス文字列2箇所)
- Delete(存在すれば): 旧 Interop asmdef への GUID 参照

- [ ] **Step 1: Core.dll に Unity 用 .meta を付与(precompiled plugin として認識)**

Unity を開くと `CefUnity.Core.dll.meta` が自動生成される。手動同梱する場合は managed plugin 設定(`isPreloaded`/全プラットフォーム有効)の .meta を作成する。まずは Step 5 の Unity 起動で自動生成させる方針でよい。

- [ ] **Step 2: Runtime asmdef が Core.dll を参照するよう設定**

`Assets/CefUnity/Runtime/Script.asmdef` の `precompiledReferences` に `CefUnity.Core.dll` を追加し、`overrideReferences` を `true` に:
```json
"overrideReferences": true,
"precompiledReferences": [ "CefUnity.Core.dll" ],
```
(managed plugin が `autoReferenced` 扱いなら追加不要だが、明示が安全。Editor asmdef も同様に Core.dll 参照が要る場合は同設定を追加。)

- [ ] **Step 3: Editor ScrollReplay.cs を Core 呼び出しの薄ラッパへ**

`Assets/CefUnity/Editor/ScrollReplay.cs` の中身を次へ置換(I/O・ログ・Exit のみ Unity 側、判定は Core へ委譲。`#if CEF_UNITY_DEV_TOOLS` ガードは維持):
```csharp
// この開発リポジトリ専用ツール (CEF_UNITY_DEV_TOOLS)。パッケージ利用側では無効。
#if CEF_UNITY_DEV_TOOLS
using CefUnity.Runtime;
using UnityEditor;
using UnityEngine;

namespace CefUnity.Editor
{
    /// <summary>
    ///   開発用: cef_scroll_record 録画を ScrollReplayRunner(Core)でオフライン検証する。
    ///   batchmode: Unity -batchmode -quit -executeMethod CefUnity.Editor.ScrollReplay.Run
    ///   入力 $TMPDIR/cef_scroll_events.csv → 出力 $TMPDIR/cef_scroll_replay.csv。
    /// </summary>
    public static class ScrollReplay
    {
        public static void Run()
        {
            var tmp = System.IO.Path.GetTempPath();
            var src = System.IO.Path.Combine(tmp, "cef_scroll_events.csv");
            var dst = System.IO.Path.Combine(tmp, "cef_scroll_replay.csv");
            if (!System.IO.File.Exists(src))
            {
                Debug.LogError($"[ScrollReplay] input not found: {src} — cef_scroll_record トグルで録画してから実行すること");
                if (Application.isBatchMode) EditorApplication.Exit(1);
                return;
            }
            var result = ScrollReplayRunner.Run(System.IO.File.ReadLines(src));
            if (!result.Ok)
            {
                Debug.LogError($"[ScrollReplay] {result.Error}");
                if (Application.isBatchMode) EditorApplication.Exit(1);
                return;
            }
            System.IO.File.WriteAllText(dst, string.Join("\n", result.OutLines) + "\n");
            Debug.Log($"[ScrollReplay] events={result.Events} ticks={result.Ticks} fidelity: mismatches={result.Mismatches}/{result.Ticks} out={dst}");
        }
    }
}
#endif
```

- [ ] **Step 4: CefBuildPostProcessor のパスを新 Plugins 位置へ更新**

`Assets/CefUnity/Editor/CefBuildPostProcessor.cs`:
- macOS(現 45–47 行):
  ```csharp
  var src = Path.Combine(Application.dataPath, "CefUnity", "Plugins", "osx-arm64", "cef-unity-server.app");
  ```
- Windows(現 90–92 行):
  ```csharp
  var src = Path.Combine(Application.dataPath, "CefUnity", "Plugins", "win-x64");
  ```
(`"Interop", "Plugins"` → `"Plugins"` の2箇所のみ。ロジック・`callbackOrder` は不変。)

- [ ] **Step 5: Unity を開いてコンパイル・再生・ビルドを検証(手動/バッチ)**

Run(Editor 終了状態で):
```bash
cd /Users/juha/Documents/GitHub/cef-unity
"/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity" \
  -batchmode -quit -projectPath cef-unity-unityproject \
  -executeMethod CefUnity.Editor.CefQuickBuild.BuildMac \
  -logFile /tmp/unity-rewire.log
echo "exit=$?"
tail -40 /tmp/unity-rewire.log
```
Expected: コンパイルエラーなし、`build-mac/CefUnity.app` 生成、post-processor が `Plugins/osx-arm64/cef-unity-server.app` をコピー成功のログ。exit=0。
(手動確認: Editor で Play → ページ描画・スクロール・音声が従来どおり。ScrollReplay batchmode `CefUnity.Editor.ScrollReplay.Run` も回ること。)

- [ ] **Step 6: コミット**

```bash
git add -A cef-unity-unityproject/Assets/CefUnity/Runtime/Script.asmdef \
           cef-unity-unityproject/Assets/CefUnity/Editor
git commit -m "refactor(unity): Runtime を Core.dll 参照へ再配線・ScrollReplay 薄ラッパ化・postprocessor パス更新" --author="Juha <sakastudio@moores.tech>"
```

---

## Phase 6 — CI 連携

### Task 6.1: rust-build.yml に dotnet test を追加

**Files:**
- Modify: `.github/workflows/rust-build.yml`

- [ ] **Step 1: 現行ワークフローの job 構成を確認**

Run: `sed -n '1,80p' /Users/juha/Documents/GitHub/cef-unity/.github/workflows/rust-build.yml`
Expected: mac/win ビルド job 名と `runs-on`、既存の `cargo test --lib` ステップ位置を把握。

- [ ] **Step 2: dotnet test ステップを追加(mac job、Core は netstandard2.1 で OS 非依存だが dylib 不要のロジックテストのみ)**

mac job の cargo テスト後に追加:
```yaml
      - name: Setup .NET
        uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '11.0.x'
      - name: Core unit tests (no Unity)
        run: dotnet test core/CefUnity.Tests -c Release --logger "console;verbosity=minimal"
```
(Tests は Core ロジックのみでネイティブ dylib 不要。`ScrollDroughtRecordingTests` の fixtures はリポジトリ同梱なので CI で解決する。)

- [ ] **Step 3: ローカルでワークフロー YAML の妥当性を確認**

Run: `cd /Users/juha/Documents/GitHub/cef-unity && python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/rust-build.yml')); print('yaml ok')"`
Expected: `yaml ok`。

- [ ] **Step 4: コミット & push(CI を発火)**

```bash
git commit -am "ci: Core 単体テスト (dotnet test) を rust-build に追加" --author="Juha <sakastudio@moores.tech>"
git push
```

- [ ] **Step 5: CI 結果を確認**

Run: `gh run watch $(gh run list --workflow rust-build.yml -L1 --json databaseId -q '.[0].databaseId') --exit-status`
Expected: mac job の `Core unit tests` が Passed、全 job green。

### Task 6.2: publish 経路に Core.dll 再生成を組み込む

**Files:**
- Modify: `.github/workflows/rust-build.yml`(publish job)

- [ ] **Step 1: publish job に Core.dll ビルド&コミットを追加**

publish job(タグ/dispatch 時)で、Plugins 配置前に:
```yaml
      - name: Build & stage CefUnity.Core.dll
        run: |
          bash core/build-core.sh
          git add cef-unity-unityproject/Assets/CefUnity/Plugins/CefUnity.Core.dll
```
(既存の LFS push + main 自動コミットステップに Core.dll を含める。ループ防止の paths フィルタ/GITHUB_TOKEN 非連鎖は既存踏襲。)

- [ ] **Step 2: タグ経路のドライラン検討(実push は任意)**

Run: 既存のリリース手順(`package.json`/`bundleVersion` を上げて `git tag vX.Y.Z && git push origin vX.Y.Z`)に沿って検証する場合のみ実施。CI の publish job で Core.dll が LFS コミットに含まれることをログで確認。

- [ ] **Step 3: コミット**

```bash
git commit -am "ci: publish 経路で CefUnity.Core.dll を再生成し LFS コミットへ含める" --author="Juha <sakastudio@moores.tech>"
```

---

## 完了条件(受け入れ基準)

1. `dotnet test core/CefUnity.Tests` が **Unity 起動なしで 60 件 green**(主目的)。
2. `dotnet run --project core/CefUnity.Harness -- smoke` が実 CEF を起動しフレーム取得(`SMOKE_OK frames>0`)。
3. `dotnet run --project core/CefUnity.Harness -- replay <録画>` が既存 batchmode と同等の判定を返す。
4. Unity(Editor)が Core.dll を参照して**コンパイル成功・Play で描画/スクロール/音声が従来どおり**、mac/win プレイヤービルドが成立し post-processor がネイティブ実行時を新 Plugins パスからコピーできる。
5. `Assets/CefUnity/Interop/`(マネージド + asmdef)が存在せず、Interop は Core.dll のみに存在。ネイティブ dylib は別ファイルのまま LFS 管理。
6. CI(rust-build.yml)で `dotnet test` が回り green。
