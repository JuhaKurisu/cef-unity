#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

# Windows x64 用のビルド + Unity プラグインへのコピー。
# cef-unity-rust.dll (cdylib), cef-unity-server.exe, cef-unity-rust-helper.exe,
# および CEF ランタイム (libcef.dll, *.pak, *.dat, locales/ 等) を一括配置する。
#
# 前提: 呼び出し元は MSVC link.exe / cl.exe にパスが通っていること。
#       Visual Studio Build Tools 2022 がある場合は vcvars64.bat を先に実行する。

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ScriptDir

$Dest = Join-Path $ScriptDir '..\cef-unity-unityproject\Assets\CefUnity\Interop\Plugins\win-x64'
$Dest = [System.IO.Path]::GetFullPath($Dest)

Write-Host "[deploy] cargo build --release"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# ---- 配置先ディレクトリを準備 ----
if (-not (Test-Path $Dest)) {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
}

# ---- Rust 成果物 ----
$Release = Join-Path $ScriptDir 'target\release'

$Artifacts = @(
    'cef_unity_rust.dll',
    'cef-unity-server.exe',
    'cef-unity-rust-helper.exe'
)
foreach ($a in $Artifacts) {
    $src = Join-Path $Release $a
    if (-not (Test-Path $src)) { throw "missing artifact: $src" }
    Copy-Item -Path $src -Destination $Dest -Force
    Write-Host "[deploy] copied $a"
}

# ---- CEF ランタイムを cef-dll-sys のビルド出力から拾う ----
# cef-rs は target/release/build/cef-dll-sys-*/out/cef_windows_x86_64/ に
# フラット展開する (Release/ や Resources/ サブフォルダなし)。
$CefDir = $null
$CefOutCandidates = Get-ChildItem -Path (Join-Path $Release 'build') -Directory -Filter 'cef-dll-sys-*' -ErrorAction SilentlyContinue
foreach ($c in $CefOutCandidates) {
    $maybe = Get-ChildItem -Path (Join-Path $c.FullName 'out') -Directory -Filter 'cef_windows*' -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($maybe -and (Test-Path (Join-Path $maybe.FullName 'libcef.dll'))) {
        $CefDir = $maybe.FullName
        break
    }
}
if (-not $CefDir) {
    throw "CEF runtime not found at target/release/build/cef-dll-sys-*/out/cef_windows*/libcef.dll"
}
Write-Host "[deploy] CEF runtime: $CefDir"

# ---- ランタイム必須 dll (Chromium / Skia / Angle / SwiftShader / Vulkan) ----
$RuntimeDlls = @(
    'libcef.dll',
    'chrome_elf.dll',
    'd3dcompiler_47.dll',
    'dxcompiler.dll',
    'dxil.dll',
    'libEGL.dll',
    'libGLESv2.dll',
    'vk_swiftshader.dll',
    'vulkan-1.dll'
)
foreach ($dll in $RuntimeDlls) {
    $src = Join-Path $CefDir $dll
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $Dest -Force
    } else {
        Write-Warning "[deploy] missing runtime dll (skipped): $dll"
    }
}

# ---- リソース (V8 snapshot / ICU / pak / SwiftShader manifest) ----
$ResourceFiles = @(
    'icudtl.dat',
    'v8_context_snapshot.bin',
    'snapshot_blob.bin',
    'resources.pak',
    'chrome_100_percent.pak',
    'chrome_200_percent.pak',
    'vk_swiftshader_icd.json'
)
foreach ($f in $ResourceFiles) {
    $src = Join-Path $CefDir $f
    if (Test-Path $src) {
        Copy-Item -Path $src -Destination $Dest -Force
    }
}

# ---- locales/ ----
$LocalesSrc = Join-Path $CefDir 'locales'
if (Test-Path $LocalesSrc) {
    $LocalesDst = Join-Path $Dest 'locales'
    if (Test-Path $LocalesDst) { Remove-Item -Path $LocalesDst -Recurse -Force }
    Copy-Item -Path $LocalesSrc -Destination $LocalesDst -Recurse -Force
}

Write-Host "[deploy] done -> $Dest"
