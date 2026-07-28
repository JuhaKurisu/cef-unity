# CefUnity.Viewer Windows D3D11 対応 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `CefUnity.Viewer` を Windows で動かし、macOS と同じ GPU ゼロコピー経路 (D3D11 共有テクスチャ + 共有 fence) でページを表示・操作できるようにする。

**Architecture:** Rust client に外部 D3D11 デバイス注入 FFI を 1 本追加し、Viewer が自前の `ID3D11Device` を作って **Browser 生成前に** 注入する。表示は `IFrameRenderer` の新実装 `D3D11FrameRenderer` が DXGI スワップチェーンへコピーして `Present(1,0)` する。プラットフォーム分岐は `FrameRendererFactory` / `CefFrameSource` / `Program` の 3 箇所に封じ込める。

**Tech Stack:** .NET 10 / Silk.NET 2.22.0 (Windowing.Sdl, Direct3D11, DXGI) / Rust edition 2024 (windows crate) / csbindgen

**Spec:** `docs/superpowers/specs/2026-07-28-windows-viewer-d3d11-design.md`

## Global Constraints

- 識別子は省略形を使わずフルネーム (`CLAUDE.md` の命名規約)。`width`/`height`/`index` 等。維持してよい頭字語: `id`, `gpu`, `fps`, `ipc`, `ffi`, `osr`, `cef`, `d3d11`, `bgra`, `hwnd`
- 文字列リテラル (dev トグル、CSV フォーマット、CLI 引数、環境変数名) は挙動契約なので変更しない
- macOS の既存挙動を一切変えない。Windows 分岐は追加のみ
- `NativeMethods.g.cs` は csbindgen 生成物。手で編集せず `cargo build` で再生成する
- Rust 変更後は `cef-unity-rust/CLAUDE.md` の手順に従う (`cargo build --release` → `NativeMethods.g.cs` 確認)
- Unity `Assets/CefUnity/Plugins/` 配下のバイナリはコミットしない (publish CI の責務)。ビルドで汚れたら `git checkout` で戻す
- Silk.NET は既存と同じ **2.22.0** を使う
- テスト対象にするクラスは `public` (既存 `ViewerOptions` / `ClickCounter` / `SilkKeyboardMapper` と同じ。`InternalsVisibleTo` は使っていない)

---

### Task 1: Windows でビルド・起動できる状態にする

**Files:**
- Modify: `cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj:15-23`
- Modify: `cef-unity-csharp/build-csharp.sh:5-7`
- Modify: `cef-unity-csharp/CefUnity.Viewer/MacMomentumScrollSupport.cs:34-51`
- Modify: `cef-unity-csharp/CefUnity.Viewer/MacApplicationActivator.cs:33-49`
- Test: `cef-unity-csharp/CefUnity.Tests/MacIntegrationGuardTests.cs` (新規)

**Interfaces:**
- Consumes: なし
- Produces: `MacMomentumScrollSupport.Enable()` と `MacApplicationActivator.ActivateCurrentApplication()` が非 macOS では何もせず戻る (`public static` へ変更)

**背景:** `MacMomentumScrollSupport.Enable()` は `NativeLibrary.Load("/System/Library/Frameworks/Foundation.framework/Foundation")` を try/catch なしで呼ぶため、Windows では `Program.cs:73` で `DllNotFoundException` が出て起動できない。`CefUnity.Harness` は dylib を無条件コピーするため Windows で `MSB3030` になる。

- [ ] **Step 1: 失敗するテストを書く**

```csharp
// cef-unity-csharp/CefUnity.Tests/MacIntegrationGuardTests.cs
using System.Runtime.InteropServices;
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class MacIntegrationGuardTests
    {
        [Test]
        public void Enable_OnNonMacOS_DoesNotThrow()
        {
            if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
                Assert.Ignore("macOS では実際に NSUserDefaults を触るため対象外");
            Assert.DoesNotThrow(() => MacMomentumScrollSupport.Enable());
        }

        [Test]
        public void ActivateCurrentApplication_OnNonMacOS_DoesNotThrow()
        {
            if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))
                Assert.Ignore("macOS では実際に NSApplication を触るため対象外");
            Assert.DoesNotThrow(() => MacApplicationActivator.ActivateCurrentApplication());
        }
    }
}
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests -c Release --filter MacIntegrationGuardTests`
Expected: コンパイルエラー (`MacMomentumScrollSupport` は `internal`)。`public` 化後は `Enable_OnNonMacOS_DoesNotThrow` が `DllNotFoundException` で FAIL

- [ ] **Step 3: OS ガードを実装**

`MacMomentumScrollSupport.cs`: クラス宣言を `public static class` にし、`Enable()` の先頭に追加:

```csharp
        public static void Enable()
        {
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.OSX)) return;

            NativeLibrary.Load("/System/Library/Frameworks/Foundation.framework/Foundation");
```

`MacApplicationActivator.cs`: クラス宣言を `public static class` にし、`ActivateCurrentApplication()` の先頭に追加:

```csharp
        public static void ActivateCurrentApplication()
        {
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.OSX)) return;
            try
```

- [ ] **Step 4: テストが通ることを確認**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests -c Release --filter MacIntegrationGuardTests`
Expected: PASS (2 件)

- [ ] **Step 5: Harness の csproj にガードを付ける**

`CefUnity.Harness.csproj` の `<None Include>` と `CopyServerApp` を Viewer と同じ条件にする:

```xml
  <ItemGroup>
    <None Include="$(RustTargetDir)/libcef_unity_rust.dylib"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.dylib"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And Exists('$(RustTargetDir)/libcef_unity_rust.dylib')" />
  </ItemGroup>
  <!-- cef-unity-server.app bundle を出力先に用意(旧 Sandbox と同じ) -->
  <Target Name="CopyServerApp" AfterTargets="Build"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And Exists('$(RustTargetDir)/cef-unity-server')">
    <Exec Command="bash '$(RustProjectDir)/build-server-sandbox.sh' '$(OutputPath)'" />
  </Target>
```

- [ ] **Step 6: build-csharp.sh の出力パスを固定**

環境変数 `Platform=x64` があるマシンでは出力が `bin/x64/Release/` になり `cp` が失敗する。`-p:Platform=AnyCPU` を明示する:

```bash
dotnet build "$HERE/CefUnity.Core/CefUnity.Core.csproj" -c Release -p:Platform=AnyCPU -v quiet
SRC="$HERE/CefUnity.Core/bin/Release/netstandard2.1/CefUnity.Core.dll"
```

- [ ] **Step 7: Windows で全ビルドとテストを確認**

Run:
```bash
dotnet build cef-unity-csharp/CefUnity.Harness -c Release
dotnet test cef-unity-csharp/CefUnity.Tests -c Release --logger "console;verbosity=minimal"
git status --short
```
Expected: Harness がビルド成功、テスト 103 件 PASS、`Assets/CefUnity/Plugins/CefUnity.Core.dll` が変更されていたら `git checkout` で戻す

- [ ] **Step 8: コミット**

```bash
git add cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj cef-unity-csharp/build-csharp.sh \
        cef-unity-csharp/CefUnity.Viewer/MacMomentumScrollSupport.cs \
        cef-unity-csharp/CefUnity.Viewer/MacApplicationActivator.cs \
        cef-unity-csharp/CefUnity.Tests/MacIntegrationGuardTests.cs
git commit -m "fix(viewer): mac 専用処理に OS ガードを付け Windows でビルド・起動可能にする"
```

---

### Task 2: 外部 D3D11 デバイス注入 FFI

**Files:**
- Modify: `cef-unity-rust/crates/client/src/d3d11.rs` (`set_unity_interfaces` の隣に追加)
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (`cef_unity_is_d3d11_connected` の直前に追加)
- Modify: `cef-unity-csharp/CefUnity.Core/Interop/CefUnity.cs` (`CefRuntime` に static ラッパ)
- 自動生成: `cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs`

**Interfaces:**
- Consumes: なし
- Produces:
  - Rust: `pub fn d3d11::set_external_device(device: *mut c_void)`
  - FFI: `cef_unity_set_external_d3d11_device(device: *mut c_void)`
  - C#: `public static void CefRuntime.SetExternalD3D11Device(IntPtr device)`

- [ ] **Step 1: d3d11.rs に注入関数を追加**

`clear_unity_interfaces` の直後に置く:

```rust
/// Unity 以外のホスト (CefUnity.Viewer 等) が自前の ID3D11Device を注入する。
/// デバイスの所有権は呼び出し側にあり、client 側は AddRef せず借用するだけなので、
/// CEF shutdown まで生存させること。
pub fn set_external_device(device: *mut c_void) {
    UNITY_DEVICE.store(device, Ordering::Release);
}
```

- [ ] **Step 2: lib.rs に FFI を追加**

`cef_unity_is_d3d11_connected` の直前に置く。csbindgen が全 OS で同じ署名を生成するよう、関数自体は cfg で分けず本体だけ分ける:

```rust
/// Windows: 外部ホストの ID3D11Device を注入する (Unity 以外のホスト用)。
///
/// **`cef_unity_create_browser` より前に呼ぶこと。** browser 生成時に共有 fence を
/// 開く判定 (`is_d3d11_connected`) が走るため、後から注入すると GPU 同期が張られない。
///
/// デバイスの所有権は呼び出し側。client は AddRef せず借用するだけなので、
/// `cef_unity_shutdown` まで生存させること。非 Windows では何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn cef_unity_set_external_d3d11_device(device: *mut std::ffi::c_void) {
    #[cfg(target_os = "windows")]
    {
        d3d11::set_external_device(device);
        log_to_file(&format!("external d3d11 device set: {:p}", device));
    }
    #[cfg(not(target_os = "windows"))]
    let _ = device;
}
```

- [ ] **Step 3: ビルドして csbindgen 再生成を確認**

Run: `cd cef-unity-rust && cargo build --release`
Expected: ビルド成功。`git diff --stat cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs` に `cef_unity_set_external_d3d11_device` の追加が出る

- [ ] **Step 4: C# ラッパを追加**

`Interop/CefUnity.cs` の `IsD3D11Connected()` の直前に追加 (`UNITY_DEVICE` はプロセスグローバルなので static):

```csharp
        /// <summary>
        ///     Windows: 外部ホストの ID3D11Device を native 側へ注入する。
        ///     Browser 生成より前に呼ぶこと (共有 fence の open 判定がそこで走るため)。
        ///     デバイスの所有権は呼び出し側にあり、CefRuntime.Shutdown まで生存させること。
        ///     非 Windows では何もしない。
        /// </summary>
        public static unsafe void SetExternalD3D11Device(IntPtr device)
        {
            NativeMethods.cef_unity_set_external_d3d11_device((void*)device);
        }
```

配置先は `IsD3D11Connected` と同じクラス。`IsD3D11Connected` が `Browser` にあるか `CefRuntime` にあるかを確認し、**static メンバが並んでいる方**に合わせる。

- [ ] **Step 5: ビルド確認**

Run: `dotnet build cef-unity-csharp/CefUnity.Core -c Release -p:Platform=AnyCPU`
Expected: 成功

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/d3d11.rs cef-unity-rust/crates/client/src/lib.rs \
        cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs \
        cef-unity-csharp/CefUnity.Core/Interop/CefUnity.cs
git commit -m "feat(ffi): 外部 D3D11 デバイス注入 cef_unity_set_external_d3d11_device を追加"
```

---

### Task 3: Windows ランタイムを Viewer の出力先へ配置

**Files:**
- Create: `cef-unity-rust/copy-windows-runtime.ps1`
- Modify: `cef-unity-rust/deploy.ps1:26-124` (収集ロジックを切り出して呼ぶ)
- Modify: `cef-unity-csharp/CefUnity.Viewer/CefUnity.Viewer.csproj` (Windows 配置ターゲット)

**Interfaces:**
- Consumes: なし
- Produces: `copy-windows-runtime.ps1 -Destination <path>` が `cef_unity_rust.dll` / `cef-unity-server.exe` / `cef-unity-rust-helper.exe` / CEF ランタイム一式をフラット配置する

**背景:** `cef-unity-server.exe` は `cef_unity_rust.dll` と同じディレクトリ直下から起動される (`crates/client/src/lib.rs:114-117`)。

- [ ] **Step 1: 収集スクリプトを切り出す**

`cef-unity-rust/copy-windows-runtime.ps1` を新規作成。`deploy.ps1:26-99` のロジックを `-Destination` 引数を取る形に移し、`cargo build` と Unity `.meta` の退避処理は含めない (それらは `deploy.ps1` に残す):

```powershell
#Requires -Version 5.1
# Rust 成果物と CEF ランタイムを指定ディレクトリへフラット配置する。
# deploy.ps1 (Unity 用) と CefUnity.Viewer.csproj (ビルド出力用) の両方から呼ばれる。
# ソースが無い場合はエラーにせず警告のみ (Rust 未ビルド環境で dotnet build を壊さないため)。
param(
    [Parameter(Mandatory = $true)][string]$Destination
)
$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Release = Join-Path $ScriptDir 'target\release'
$Destination = [System.IO.Path]::GetFullPath($Destination)

if (-not (Test-Path $Release)) {
    Write-Warning "[copy-windows-runtime] target/release が無いのでスキップ: $Release"
    exit 0
}
if (-not (Test-Path $Destination)) {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
}

$Artifacts = @('cef_unity_rust.dll', 'cef-unity-server.exe', 'cef-unity-rust-helper.exe')
foreach ($artifact in $Artifacts) {
    $source = Join-Path $Release $artifact
    if (Test-Path $source) {
        Copy-Item -Path $source -Destination $Destination -Force
    } else {
        Write-Warning "[copy-windows-runtime] missing artifact (skipped): $source"
    }
}

$CefDirectory = $null
$Candidates = Get-ChildItem -Path (Join-Path $Release 'build') -Directory -Filter 'cef-dll-sys-*' -ErrorAction SilentlyContinue
foreach ($candidate in $Candidates) {
    $maybe = Get-ChildItem -Path (Join-Path $candidate.FullName 'out') -Directory -Filter 'cef_windows*' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($maybe -and (Test-Path (Join-Path $maybe.FullName 'libcef.dll'))) {
        $CefDirectory = $maybe.FullName
        break
    }
}
if (-not $CefDirectory) {
    Write-Warning "[copy-windows-runtime] CEF ランタイムが見つからないのでスキップ"
    exit 0
}

$RuntimeDlls = @(
    'libcef.dll', 'chrome_elf.dll', 'd3dcompiler_47.dll', 'dxcompiler.dll', 'dxil.dll',
    'libEGL.dll', 'libGLESv2.dll', 'vk_swiftshader.dll', 'vulkan-1.dll'
)
$ResourceFiles = @(
    'icudtl.dat', 'v8_context_snapshot.bin', 'snapshot_blob.bin',
    'resources.pak', 'chrome_100_percent.pak', 'chrome_200_percent.pak', 'vk_swiftshader_icd.json'
)
foreach ($name in ($RuntimeDlls + $ResourceFiles)) {
    $source = Join-Path $CefDirectory $name
    if (Test-Path $source) { Copy-Item -Path $source -Destination $Destination -Force }
}

$LocalesSource = Join-Path $CefDirectory 'locales'
$LocalesDestination = Join-Path $Destination 'locales'
if (Test-Path $LocalesSource) {
    if (-not (Test-Path $LocalesDestination)) {
        New-Item -ItemType Directory -Path $LocalesDestination -Force | Out-Null
    }
    Copy-Item -Path (Join-Path $LocalesSource '*') -Destination $LocalesDestination -Recurse -Force
}
Write-Host "[copy-windows-runtime] done -> $Destination"
```

- [ ] **Step 2: deploy.ps1 から呼ぶ形にする**

`deploy.ps1` の Rust 成果物コピー〜`locales/` コピーまでを削除し、`.meta` 退避 → 共通スクリプト呼び出し → `.meta` 復元 の順に組み替える。`cargo build --release` と `throw "missing artifact"` の厳格チェック (Unity 配置では成果物欠落を検出したい) は `deploy.ps1` 側に残す:

```powershell
foreach ($a in $Artifacts) {
    $src = Join-Path $Release $a
    if (-not (Test-Path $src)) { throw "missing artifact: $src" }
}
& (Join-Path $ScriptDir 'copy-windows-runtime.ps1') -Destination $Dest
```

- [ ] **Step 3: Viewer csproj に配置ターゲットを追加**

`CefUnity.Viewer.csproj` の `CopyServerApp` ターゲットの直後に追加:

```xml
  <!-- Windows: cef-unity-server.exe は cef_unity_rust.dll と同じディレクトリ直下から
       起動されるため、Rust 成果物と CEF ランタイムを出力先へフラット配置する。
       Rust 未ビルド環境ではスクリプト側が警告のみで抜ける。 -->
  <Target Name="CopyWindowsRuntime" AfterTargets="Build"
          Condition="$([MSBuild]::IsOSPlatform('Windows'))">
    <Exec Command="powershell -NoProfile -ExecutionPolicy Bypass -File &quot;$(RustProjectDir)\copy-windows-runtime.ps1&quot; -Destination &quot;$(OutputPath)&quot;" />
  </Target>
```

- [ ] **Step 4: 配置を確認**

Run:
```bash
dotnet build cef-unity-csharp/CefUnity.Viewer -c Release
ls cef-unity-csharp/CefUnity.Viewer/bin/*/Release/net10.0/ | grep -E "cef_unity_rust.dll|cef-unity-server.exe|libcef.dll"
```
Expected: 3 ファイルとも存在する (`bin/x64/Release/` になる場合あり)

- [ ] **Step 5: コミット**

```bash
git add cef-unity-rust/copy-windows-runtime.ps1 cef-unity-rust/deploy.ps1 \
        cef-unity-csharp/CefUnity.Viewer/CefUnity.Viewer.csproj
git commit -m "build(viewer): Windows ランタイム配置スクリプトを切り出し Viewer 出力先にも配置する"
```

---

### Task 4: D3D11GraphicsDevice と FrameRendererFactory

**Files:**
- Create: `cef-unity-csharp/CefUnity.Viewer/D3D11GraphicsDevice.cs`
- Create: `cef-unity-csharp/CefUnity.Viewer/FrameRendererFactory.cs`
- Modify: `cef-unity-csharp/CefUnity.Viewer/CefUnity.Viewer.csproj` (PackageReference 2 件)
- Test: `cef-unity-csharp/CefUnity.Tests/FrameRendererFactoryTests.cs` (新規)

**Interfaces:**
- Consumes: `CefRuntime.SetExternalD3D11Device(IntPtr)` (Task 2)
- Produces:
  - `public enum FrameRendererKind { Metal, Direct3D11, Unsupported }`
  - `public static FrameRendererKind FrameRendererFactory.SelectKind(bool isMacOS, bool isWindows)`
  - `internal sealed class D3D11GraphicsDevice : IDisposable` — `unsafe ID3D11Device* Device`, `unsafe ID3D11DeviceContext* ImmediateContext`, `IntPtr DevicePointer`

- [ ] **Step 1: 失敗するテストを書く**

```csharp
// cef-unity-csharp/CefUnity.Tests/FrameRendererFactoryTests.cs
using CefUnity.Viewer;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class FrameRendererFactoryTests
    {
        [Test]
        public void SelectKind_MacOS_ReturnsMetal()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: true, isWindows: false),
                           Is.EqualTo(FrameRendererKind.Metal));

        [Test]
        public void SelectKind_Windows_ReturnsDirect3D11()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: false, isWindows: true),
                           Is.EqualTo(FrameRendererKind.Direct3D11));

        [Test]
        public void SelectKind_OtherPlatform_ReturnsUnsupported()
            => Assert.That(FrameRendererFactory.SelectKind(isMacOS: false, isWindows: false),
                           Is.EqualTo(FrameRendererKind.Unsupported));
    }
}
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests -c Release --filter FrameRendererFactoryTests`
Expected: コンパイルエラー (`FrameRendererFactory` が存在しない)

- [ ] **Step 3: パッケージ参照を追加**

`CefUnity.Viewer.csproj` の `<ItemGroup>` (PackageReference 群) に追加:

```xml
    <PackageReference Include="Silk.NET.Direct3D11" Version="2.22.0" />
    <PackageReference Include="Silk.NET.DXGI" Version="2.22.0" />
```

- [ ] **Step 4: FrameRendererFactory を実装**

```csharp
// cef-unity-csharp/CefUnity.Viewer/FrameRendererFactory.cs
using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>表示バックエンドの種別。プラットフォーム分岐はここに封じ込める。</summary>
    public enum FrameRendererKind
    {
        Metal,
        Direct3D11,
        Unsupported,
    }

    /// <summary>
    ///     実行中のプラットフォームに応じた表示バックエンドを選ぶ。
    ///     判定ロジックはテスト可能にするため OS 判定を引数で受ける。
    /// </summary>
    public static class FrameRendererFactory
    {
        public static FrameRendererKind SelectKind(bool isMacOS, bool isWindows)
        {
            if (isMacOS) return FrameRendererKind.Metal;
            if (isWindows) return FrameRendererKind.Direct3D11;
            return FrameRendererKind.Unsupported;
        }

        public static FrameRendererKind SelectKind()
            => SelectKind(
                RuntimeInformation.IsOSPlatform(OSPlatform.OSX),
                RuntimeInformation.IsOSPlatform(OSPlatform.Windows));
    }
}
```

- [ ] **Step 5: テストが通ることを確認**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests -c Release --filter FrameRendererFactoryTests`
Expected: PASS (3 件)

- [ ] **Step 6: D3D11GraphicsDevice を実装**

server (`crates/server/src/d3d11_pool.rs:127-`) と同一条件 — 既定アダプタ / `Hardware` / `BgraSupport` — でデバイスを作る。

```csharp
// cef-unity-csharp/CefUnity.Viewer/D3D11GraphicsDevice.cs
using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     Viewer が所有する ID3D11Device と immediate context。ウィンドウには依存しない
    ///     (スワップチェーンは D3D11FrameRenderer が持つ)。
    ///
    ///     native 側 (crates/client/src/d3d11.rs) はこのデバイスを AddRef せず借用するだけなので、
    ///     CEF shutdown まで Dispose してはならない。
    ///     デバイス生成条件は server 側 D3D11Pool と揃える (既定アダプタ / Hardware / BGRA_SUPPORT)。
    ///     揃えないと共有テクスチャを開けないアダプタになりうる。
    /// </summary>
    internal sealed unsafe class D3D11GraphicsDevice : IDisposable
    {
        private readonly D3D11 _d3d11;
        private ComPtr<ID3D11Device> _device;
        private ComPtr<ID3D11DeviceContext> _immediateContext;

        public D3D11GraphicsDevice()
        {
            _d3d11 = D3D11.GetApi(null);
            D3DFeatureLevel featureLevel = default;
            var result = _d3d11.CreateDevice(
                default(ComPtr<IDXGIAdapter>),
                D3DDriverType.Hardware,
                nint.Zero,
                (uint)CreateDeviceFlag.BgraSupport,
                null,
                0,
                D3D11.SdkVersion,
                ref _device,
                ref featureLevel,
                ref _immediateContext);
            SilkMarshal.ThrowHResult(result);
        }

        public ID3D11Device* Device => _device;
        public ID3D11DeviceContext* ImmediateContext => _immediateContext;
        public IntPtr DevicePointer => (IntPtr)_device.Handle;

        public void Dispose()
        {
            _immediateContext.Dispose();
            _device.Dispose();
            _d3d11.Dispose();
        }
    }
}
```

- [ ] **Step 7: ビルド確認**

Run: `dotnet build cef-unity-csharp/CefUnity.Viewer -c Release`
Expected: 成功。Silk.NET の実際の `CreateDevice` オーバーロード形に合わせて引数を調整すること (`ComPtr` の `ref` 渡し / `nint` の既定値)

- [ ] **Step 8: コミット**

```bash
git add cef-unity-csharp/CefUnity.Viewer/D3D11GraphicsDevice.cs \
        cef-unity-csharp/CefUnity.Viewer/FrameRendererFactory.cs \
        cef-unity-csharp/CefUnity.Viewer/CefUnity.Viewer.csproj \
        cef-unity-csharp/CefUnity.Tests/FrameRendererFactoryTests.cs
git commit -m "feat(viewer): D3D11 デバイス所有クラスとレンダラ選択を追加"
```

---

### Task 5: D3D11FrameRenderer

**Files:**
- Create: `cef-unity-csharp/CefUnity.Viewer/D3D11FrameRenderer.cs`

**Interfaces:**
- Consumes: `IFrameRenderer` (`Initialize(IView)` / `Present(IntPtr, int, int)` / `Dispose()`)、`D3D11GraphicsDevice` (Task 4)
- Produces: `internal sealed class D3D11FrameRenderer : IFrameRenderer` — コンストラクタは `D3D11FrameRenderer(D3D11GraphicsDevice graphicsDevice)`。受信 format を伝えるため `public void SetReceivedFormat(uint format)` を持つ (0=BGRA, 1=RGBA)

**設計上の必須事項 (spec §D3D11FrameRenderer の詳細):**
- コピーは **`D3D11GraphicsDevice.ImmediateContext`** で行う。native 側の `wait_fence` が同じ immediate context に `Wait` を積むため、別 context だと同期が壊れる
- サイズまたは format が不一致なら `ResizeBuffers` してそのフレームは skip、次フレームでコピー (Metal 側の drawableSize 収束と同じ理由)
- 受信テクスチャは native 側がキャッシュ AddRef 管理するので Release しない

- [ ] **Step 1: 実装**

```csharp
// cef-unity-csharp/CefUnity.Viewer/D3D11FrameRenderer.cs
using Silk.NET.Core.Contexts;
using Silk.NET.Core.Native;
using Silk.NET.Direct3D11;
using Silk.NET.DXGI;
using Silk.NET.Windowing;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     受信 ID3D11Texture2D を DXGI スワップチェーンのバックバッファへコピーして表示する。
    ///
    ///     色: server はプールテクスチャを B8G8R8A8_UNORM_SRGB (RGBA 時は R8G8B8A8_UNORM_SRGB) で
    ///     作る (crates/server/src/server.rs:377-384)。UNORM ↔ UNORM_SRGB は同 family なので
    ///     コピーは通り、CEF の出す sRGB エンコード済みバイトがそのままバックバッファに入る。
    ///     _UNORM のスワップチェーンで無変換表示すると正しく見える (Metal 側の blit と同じ理屈)。
    ///     ただし BGRA と RGBA は family が異なりコピーが失敗するため、受信 format タグを
    ///     追跡してスワップチェーンを作り直す。
    ///
    ///     サイズ収束: バックバッファとテクスチャのサイズが不一致なら ResizeBuffers して
    ///     そのフレームは skip する (in-flight のバッファが旧サイズを持ちうるため)。
    ///
    ///     同期: native 側 wait_fence は注入デバイスの immediate context に Wait を積むため、
    ///     コピーも必ず同じ immediate context で行う。deferred context は作らない。
    /// </summary>
    internal sealed unsafe class D3D11FrameRenderer : IFrameRenderer
    {
        private readonly D3D11GraphicsDevice _graphicsDevice;
        private readonly DXGI _dxgi;
        private ComPtr<IDXGISwapChain1> _swapChain;
        private nint _windowHandle;
        private int _bufferWidth;
        private int _bufferHeight;
        private uint _bufferFormatTag;
        private uint _receivedFormatTag;

        public D3D11FrameRenderer(D3D11GraphicsDevice graphicsDevice)
        {
            _graphicsDevice = graphicsDevice;
            _dxgi = DXGI.GetApi(null);
        }

        /// <summary>受信テクスチャの format タグ (0=BGRA, 1=RGBA) を伝える。</summary>
        public void SetReceivedFormat(uint format) => _receivedFormatTag = format;

        private static Format ToDxgiFormat(uint formatTag)
            => formatTag == 1 ? Format.FormatR8G8B8A8Unorm : Format.FormatB8G8R8A8Unorm;

        public void Initialize(IView view)
        {
            var win32 = view.Native?.Win32
                        ?? throw new InvalidOperationException("Win32 native window handle not available");
            _windowHandle = win32.Value.Hwnd;
            CreateSwapChain(Math.Max(view.Size.X, 1), Math.Max(view.Size.Y, 1), _receivedFormatTag);
        }

        private void CreateSwapChain(int width, int height, uint formatTag)
        {
            _swapChain.Dispose();
            _swapChain = default;

            ComPtr<IDXGIFactory2> factory = default;
            SilkMarshal.ThrowHResult(_dxgi.CreateDXGIFactory2(0, out factory));
            try
            {
                var description = new SwapChainDesc1
                {
                    Width = (uint)width,
                    Height = (uint)height,
                    Format = ToDxgiFormat(formatTag),
                    BufferCount = 2,
                    BufferUsage = DXGI.UsageRenderTargetOutput,
                    SwapEffect = SwapEffect.FlipDiscard,
                    SampleDesc = new SampleDesc(1, 0),
                    Scaling = Scaling.Stretch,
                    AlphaMode = Silk.NET.DXGI.AlphaMode.Ignore,
                };
                ComPtr<IDXGISwapChain1> swapChain = default;
                SilkMarshal.ThrowHResult(factory.CreateSwapChainForHwnd(
                    (IUnknown*)_graphicsDevice.Device,
                    _windowHandle,
                    in description,
                    null,
                    ref Unsafe.NullRef<IDXGIOutput>(),
                    ref swapChain));
                _swapChain = swapChain;
                _bufferWidth = width;
                _bufferHeight = height;
                _bufferFormatTag = formatTag;
            }
            finally
            {
                factory.Dispose();
            }
        }

        public void Present(IntPtr texturePointer, int width, int height)
        {
            if (_swapChain.Handle == null) return;

            if (texturePointer != IntPtr.Zero && width > 0 && height > 0)
            {
                // format が変わったらスワップチェーンごと作り直す (family 違いはコピー不可)
                if (_receivedFormatTag != _bufferFormatTag)
                {
                    CreateSwapChain(width, height, _receivedFormatTag);
                    return;
                }
                // サイズ収束: 一致するまではコピーせず次フレームに回す
                if (_bufferWidth != width || _bufferHeight != height)
                {
                    SilkMarshal.ThrowHResult(_swapChain.ResizeBuffers(
                        0, (uint)width, (uint)height, Format.FormatUnknown, 0));
                    _bufferWidth = width;
                    _bufferHeight = height;
                    return;
                }

                ComPtr<ID3D11Texture2D> backBuffer = default;
                SilkMarshal.ThrowHResult(_swapChain.GetBuffer(0, out backBuffer));
                try
                {
                    _graphicsDevice.ImmediateContext->CopySubresourceRegion(
                        (ID3D11Resource*)backBuffer.Handle, 0, 0, 0, 0,
                        (ID3D11Resource*)texturePointer, 0, null);
                }
                finally
                {
                    backBuffer.Dispose();
                }
            }

            // vsync 待ち。mac の CAMetalLayer displaySync 相当のフレームペーシングを担う
            _swapChain.Present(1, 0);
        }

        public void Dispose()
        {
            _swapChain.Dispose();
            _swapChain = default;
            _dxgi.Dispose();
        }
    }
}
```

- [ ] **Step 2: ビルド確認**

Run: `dotnet build cef-unity-csharp/CefUnity.Viewer -c Release`
Expected: 成功。Silk.NET 2.22.0 の実シグネチャ (`CreateSwapChainForHwnd` の out パラメータ形、`GetBuffer` のジェネリック形) に合わせて調整すること

- [ ] **Step 3: コミット**

```bash
git add cef-unity-csharp/CefUnity.Viewer/D3D11FrameRenderer.cs
git commit -m "feat(viewer): D3D11 スワップチェーン表示を実装"
```

---

### Task 6: 配線 (Program / ViewerWindow / CefFrameSource / SpikeRunner)

**Files:**
- Modify: `cef-unity-csharp/CefUnity.Viewer/CefFrameSource.cs:22-41`
- Modify: `cef-unity-csharp/CefUnity.Viewer/ViewerWindow.cs:43-68,109-143`
- Modify: `cef-unity-csharp/CefUnity.Viewer/Program.cs:9-16,75-97`
- Modify: `cef-unity-csharp/CefUnity.Viewer/SpikeRunner.cs:20-68`

**Interfaces:**
- Consumes: `FrameRendererFactory.SelectKind()`、`D3D11GraphicsDevice`、`D3D11FrameRenderer`、`CefRuntime.SetExternalD3D11Device`
- Produces:
  - `ViewerWindow(ViewerOptions, CefFrameSource, ScrollInputMatrix, Func<Sdl, IFrameRenderer>, StatisticsRecorder?, ScrollReplaySource?)` — 第 4 引数にレンダラ生成関数が入る。`MetalFrameRenderer` は `Sdl` インスタンスを要求し、それは `ViewerWindow` がウィンドウを作った後にしか取れないため、インスタンスではなくファクトリを渡す
  - `CefFrameSource.TickFrame(out IntPtr texturePointer, out int width, out int height, out uint format)` — format を追加

- [ ] **Step 1: CefFrameSource をプラットフォーム分岐にする**

`TickFrame` を書き換え、`format` を out で返す (D3D11FrameRenderer が family 判定に使う)。Windows 経路では受信テクスチャを解放しない (native 側がキャッシュ管理):

```csharp
        /// <summary>毎フレーム 1 回。新フレームが無ければ直前のテクスチャを返し続ける。</summary>
        public bool TickFrame(out IntPtr texturePointer, out int width, out int height, out uint format)
        {
            _browser.SendExternalBeginFrame(_frameIndex++);
            CefRuntime.Pump();
            if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            {
                // Windows: 返るポインタは native 側のキャッシュ (AddRef 管理) なので解放しない
                if (_browser.TryReceiveD3D11Texture(out var d3d11Texture, out var d3d11Width, out var d3d11Height, out var d3d11Format))
                {
                    _currentTexture = d3d11Texture;
                    _textureWidth = d3d11Width;
                    _textureHeight = d3d11Height;
                    _currentFormat = d3d11Format;
                }
            }
            else if (Browser.TryReceiveIOSurfaceTexture(out var newTexture, out var newWidth, out var newHeight, out var newFormat))
            {
                if (_currentTexture != IntPtr.Zero) Browser.ReleaseMetalTexture(_currentTexture);
                _currentTexture = newTexture;
                _textureWidth = newWidth;
                _textureHeight = newHeight;
                _currentFormat = newFormat;
            }
            texturePointer = _currentTexture;
            width = _textureWidth;
            height = _textureHeight;
            format = _currentFormat;
            return _currentTexture != IntPtr.Zero;
        }
```

`private uint _currentFormat;` フィールドを追加し、`Dispose` の `ReleaseMetalTexture` も Windows では呼ばないようガードする:

```csharp
        public void Dispose()
        {
            if (_currentTexture != IntPtr.Zero && !RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
            {
                Browser.ReleaseMetalTexture(_currentTexture);
            }
            _currentTexture = IntPtr.Zero;
            _browser.Dispose();
        }
```

- [ ] **Step 2: ViewerWindow がレンダラを受け取るようにする**

コンストラクタから `_renderer = new MetalFrameRenderer(_sdl);` を削除し、生成関数を引数で受け取る。`MetalFrameRenderer` は `Sdl` インスタンスを要求し、それは `SdlWindowing.GetExistingApi(_window)` でウィンドウ生成後にしか取れないため、インスタンスではなくファクトリを渡す:

```csharp
        public ViewerWindow(ViewerOptions options, CefFrameSource frameSource, ScrollInputMatrix scrollMatrix,
                            Func<Sdl, IFrameRenderer> rendererFactory, StatisticsRecorder? statistics,
                            CefUnity.Runtime.ScrollReplaySource? replaySource = null)
```

本体では既存の `_sdl` 取得 (`SdlWindowing.GetExistingApi(_window)`) の直後に `_renderer = rendererFactory(_sdl);` を置く。`_sdl` は IME でも使うのでフィールドのまま残す。

`OnRender` の受信呼び出しを 4 引数に合わせ、Windows のみ format を伝える:

```csharp
            if (_frameSource.TickFrame(out var texturePointer, out var textureWidth, out var textureHeight, out var textureFormat))
            {
                if (_renderer is D3D11FrameRenderer d3d11Renderer) d3d11Renderer.SetReceivedFormat(textureFormat);
                _renderer.Present(texturePointer, textureWidth, textureHeight);
```

- [ ] **Step 3: Program の初期化順序を変える**

`CefRuntime.Initialize` の後、`CefFrameSource` 生成の **前** にデバイスを作って注入する (Browser 生成時に共有 fence が開かれるため)。`SdlWindowing.Use()` は `ViewerWindow` のコンストラクタ内で呼ばれるので、レンダラ生成もそこより前で良い:

```csharp
CefRuntime.Initialize(useGpu: true);
D3D11GraphicsDevice? graphicsDevice = null;
try
{
    var rendererKind = FrameRendererFactory.SelectKind();
    if (rendererKind == FrameRendererKind.Unsupported)
    {
        Console.Error.WriteLine("このプラットフォームには表示バックエンドがありません (macOS / Windows のみ対応)");
        return 1;
    }
    if (rendererKind == FrameRendererKind.Direct3D11)
    {
        // 共有 fence は cef_unity_create_browser の中で開かれるため、
        // デバイス注入は必ず CefFrameSource (= Browser) 生成より前に行う。
        graphicsDevice = new D3D11GraphicsDevice();
        CefRuntime.SetExternalD3D11Device(graphicsDevice.DevicePointer);
        if (!Browser.IsD3D11Connected())
        {
            Console.Error.WriteLine("D3D11 デバイスの注入に失敗しました (native 側が接続を認識していません)");
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
    // native 側はデバイスを借用するだけなので、CEF shutdown 後に解放する
    CefRuntime.Shutdown();
    graphicsDevice?.Dispose();
}
return 0;
```

レンダラ生成関数と復旧手順メッセージはファイル末尾のローカル関数にする (トップレベルステートメントの後ろに置ける):

```csharp
static Func<Sdl, IFrameRenderer> RendererFactoryFor(FrameRendererKind kind, D3D11GraphicsDevice? graphicsDevice)
{
    // Metal は ViewerWindow が持つ Sdl インスタンスを要求するため、
    // 生成をウィンドウ側まで遅延させる。D3D11 は Sdl を使わない。
    if (kind == FrameRendererKind.Direct3D11)
        return _ => new D3D11FrameRenderer(graphicsDevice!);
    return sdl => new MetalFrameRenderer(sdl);
}

static string RecoveryHint()
    => RuntimeInformation.IsOSPlatform(OSPlatform.Windows)
        ? "復旧手順: サーバー残留は `taskkill /IM cef-unity-server.exe /F`、起動ハングはキャッシュ破損の可能性 → %TEMP% の cef_unity_cache を削除"
        : "復旧手順: サーバー残留は `pkill -f cef-unity-server`、起動ハングはキャッシュ破損の可能性 → $TMPDIR の cef_unity_cache を削除";
```

`graphicsDevice` は `CefRuntime.Shutdown()` の後に Dispose する。native 側はデバイスを AddRef せず借用するだけなので、CEF が使い終わる前に解放してはならない。

- [ ] **Step 4: SpikeRunner を Windows 対応にする**

`MetalFrameRenderer` と `MacNativeScrollSource` の直接生成を分岐させる。Windows では S1 (NSEvent monitor) と S2 (IME) は該当しないので、S3 相当のスワップチェーン疎通のみ確認する:

```csharp
            var rendererKind = FrameRendererFactory.SelectKind();
            D3D11GraphicsDevice? graphicsDevice = rendererKind == FrameRendererKind.Direct3D11
                ? new D3D11GraphicsDevice() : null;
            IFrameRenderer renderer = graphicsDevice != null
                ? new D3D11FrameRenderer(graphicsDevice)
                : new MetalFrameRenderer(sdl);
```

`scrollSource` の生成・`Start()`・`Poll` は `rendererKind == FrameRendererKind.Metal` のときだけ実行し、Windows では `S1 skipped (Windows: native scroll source 未対応)` を出力する。末尾で `graphicsDevice?.Dispose()` する。

- [ ] **Step 5: ビルドとテスト**

Run:
```bash
dotnet build cef-unity-csharp/CefUnity.Viewer -c Release
dotnet test cef-unity-csharp/CefUnity.Tests -c Release --logger "console;verbosity=minimal"
```
Expected: ビルド成功、テスト全件 PASS

- [ ] **Step 6: コミット**

```bash
git add cef-unity-csharp/CefUnity.Viewer/
git commit -m "feat(viewer): Windows で D3D11 経路を配線する"
```

---

### Task 7: 実機検証

**Files:** なし (検証のみ。不具合が出たら該当タスクのファイルを修正)

- [ ] **Step 1: spike で疎通確認**

Run: `dotnet run --project cef-unity-csharp/CefUnity.Viewer -c Release -- spike`
Expected: 例外なく 300 フレーム present、`S1 skipped` が出力される

- [ ] **Step 2: ページ表示を確認**

Run: `dotnet run --project cef-unity-csharp/CefUnity.Viewer -c Release -- --url https://example.com --size 1280x720`
Expected: ウィンドウにページが表示される。黒画面のままなら `%TEMP%\cef_unity_debug.log` の `external d3d11 device set` / `opened handle=` / `on_accelerated_paint` 行を確認する

- [ ] **Step 3: スクリーンショットを取得して確認**

PowerShell で前面ウィンドウを撮る (scratchpad に保存):

```powershell
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
$bitmap = New-Object System.Drawing.Bitmap $bounds.Width, $bounds.Height
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.CopyFromScreen($bounds.Location, [System.Drawing.Point]::Empty, $bounds.Size)
$bitmap.Save("$env:TEMP\viewer-windows.png")
```

Expected: ページが正しい色 (白背景・黒文字) で表示されている。色が反転/褪せている場合は format family の扱いを見直す

- [ ] **Step 4: 入力を確認**

マウス移動でホバー、リンククリック、ホイールスクロール、キーボード入力 (検索ボックス等)、ウィンドウリサイズ。
Expected: いずれも反応する。リサイズ後に絵が追従する (1 フレーム遅れて収束)

- [ ] **Step 5: プロセス残留がないことを確認**

Run: `tasklist | findstr cef-unity-server`
Expected: 何も出力されない (ウィンドウを閉じた後)

- [ ] **Step 6: 修正があればコミット**

```bash
git add -A cef-unity-csharp cef-unity-rust
git commit -m "fix(viewer): 実機検証で見つかった Windows 表示の不具合を修正"
```

---

### Task 8: CI とドキュメント

**Files:**
- Modify: `.github/workflows/rust-build.yml:67-90` (`build-win` ジョブ)
- Modify: `cef-unity-rust/CLAUDE.md:52-56`
- Modify: `cef-unity-csharp/CefUnity.Viewer/README.md:1-13,29-34`
- Modify: `docs/superpowers/specs/2026-07-25-silknet-viewer-design.md` (スコープ節)

- [ ] **Step 1: CI に Windows の C# テストを追加**

`build-win` ジョブの `Unit tests` (cargo) の後に追加 (`build-mac` の該当ステップと同じ形):

```yaml
      - uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '10.0.x'

      - name: C# tests
        run: dotnet test cef-unity-csharp/CefUnity.Tests -c Release --logger "console;verbosity=minimal"
```

- [ ] **Step 2: cef-unity-rust/CLAUDE.md の記述を更新**

「Windows ではゼロコピー GPU 経路 (IOSurface/Mach/Metal) は無効で、software paint (共有メモリ経由の BGRA 転送) で動作する。将来的な D3D11 共有テクスチャ対応はフェーズ 2 で実装予定。」を、D3D11 共有テクスチャ + 共有 fence の GPU 経路が実装済みであること、Unity 以外のホストは `cef_unity_set_external_d3d11_device` で自前デバイスを注入すること (Browser 生成前) に書き換える。

- [ ] **Step 3: Viewer README を更新**

冒頭の「mac 単体ブラウザ」を「macOS / Windows 対応の単体ブラウザ」に改め、Windows のトラブルシュートを追記:

```markdown
- サーバープロセス残留 (Windows): `taskkill /IM cef-unity-server.exe /F`
- 起動ハング (Windows、キャッシュ破損): `%TEMP%` 配下の cef_unity_cache を削除
- Windows ではネイティブスクロールソース (NSEvent 相当) が無いため、resampler モードは SDL の wheel イベントにフォールバックする
```

- [ ] **Step 4: 元 spec のスコープ節に追記**

`2026-07-25-silknet-viewer-design.md` の「対象 OS: macOS のみ (Windows は保留...)」に、`2026-07-28-windows-viewer-d3d11-design.md` で Windows 対応済みである旨の 1 行を追記する。

- [ ] **Step 5: コミットして push**

```bash
git add .github/workflows/rust-build.yml cef-unity-rust/CLAUDE.md \
        cef-unity-csharp/CefUnity.Viewer/README.md docs/superpowers/specs/
git commit -m "docs,ci: Windows の C# テストを CI に追加しドキュメントを更新"
git push
```

---

## 完了条件

- Windows で `dotnet build` が全プロジェクト成功する
- `dotnet test` が全件 PASS する (macOS / Windows 両方)
- Windows で Viewer がページを GPU 経路で表示し、マウス・ホイール・キーボード・リサイズが動作する
- ウィンドウを閉じた後に `cef-unity-server.exe` が残らない
- macOS の挙動は変わっていない (Metal 経路のコードパスに変更が入っていないこと)
