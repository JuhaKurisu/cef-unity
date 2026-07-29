#Requires -Version 5.1
# Rust 成果物と CEF ランタイムを指定ディレクトリへフラット配置する。
# deploy.ps1 (Unity 用) と CefUnity.Viewer.csproj (ビルド出力用) の両方から呼ばれる。
#
# cef-unity-server.exe は cef_unity_rust.dll と同じディレクトリ直下から起動されるため
# (crates/client/src/lib.rs の server_binary_path)、両者を同じ場所に置く必要がある。
#
# ソースが無い場合はエラーにせず警告のみ (Rust 未ビルド環境で dotnet build を壊さないため)。
# 成果物の欠落を検出したい呼び出し元 (deploy.ps1) は、呼ぶ前に自前で検査すること。
param(
    [Parameter(Mandatory = $true)][string]$Destination,
    # クロスビルド時のターゲットトリプル (例: aarch64-pc-windows-msvc)。
    # 指定すると成果物を target\<triple>\release から拾う。未指定ならホストビルドの target\release。
    [string]$Target = ''
)
$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if ($Target) {
    $Release = Join-Path $ScriptDir "target\$Target\release"
} else {
    $Release = Join-Path $ScriptDir 'target\release'
}
$Destination = [System.IO.Path]::GetFullPath($Destination)

if (-not (Test-Path $Release)) {
    Write-Warning "[copy-windows-runtime] target/release が無いのでスキップ: $Release"
    exit 0
}
if (-not (Test-Path $Destination)) {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
}

# ---- Rust 成果物 ----
$Artifacts = @('cef_unity_rust.dll', 'cef-unity-server.exe', 'cef-unity-rust-helper.exe')
foreach ($artifact in $Artifacts) {
    $source = Join-Path $Release $artifact
    if (Test-Path $source) {
        Copy-Item -Path $source -Destination $Destination -Force
    } else {
        Write-Warning "[copy-windows-runtime] missing artifact (skipped): $source"
    }
}

# ---- CEF ランタイムを cef-dll-sys のビルド出力から拾う ----
# cef-rs は target/release/build/cef-dll-sys-*/out/cef_windows_x86_64/ にフラット展開する。
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
foreach ($name in ($RuntimeDlls + $ResourceFiles)) {
    $source = Join-Path $CefDirectory $name
    if (Test-Path $source) {
        Copy-Item -Path $source -Destination $Destination -Force
    } elseif ($RuntimeDlls -contains $name) {
        Write-Warning "[copy-windows-runtime] missing runtime dll (skipped): $name"
    }
}

# ---- locales/ ----
# 呼び出し元が Unity の .meta を保持したい場合は、呼ぶ前後で自前に退避・復元すること。
$LocalesSource = Join-Path $CefDirectory 'locales'
$LocalesDestination = Join-Path $Destination 'locales'
if (Test-Path $LocalesSource) {
    if (-not (Test-Path $LocalesDestination)) {
        New-Item -ItemType Directory -Path $LocalesDestination -Force | Out-Null
    }
    Copy-Item -Path (Join-Path $LocalesSource '*') -Destination $LocalesDestination -Recurse -Force
}

Write-Host "[copy-windows-runtime] done -> $Destination"
