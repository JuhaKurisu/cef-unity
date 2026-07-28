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

$Dest = Join-Path $ScriptDir '..\cef-unity-unityproject\Assets\CefUnity\Plugins\win-x64'
$Dest = [System.IO.Path]::GetFullPath($Dest)

Write-Host "[deploy] cargo build --release"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# ---- 配置先ディレクトリを準備 ----
if (-not (Test-Path $Dest)) {
    New-Item -ItemType Directory -Path $Dest -Force | Out-Null
}

# ---- Rust 成果物の存在検査 ----
# 実コピーは copy-windows-runtime.ps1 (Viewer のビルドと共有) が行うが、
# Unity への配置では成果物の欠落を検出したいのでここで厳格に検査する。
$Release = Join-Path $ScriptDir 'target\release'

$Artifacts = @(
    'cef_unity_rust.dll',
    'cef-unity-server.exe',
    'cef-unity-rust-helper.exe'
)
foreach ($a in $Artifacts) {
    $src = Join-Path $Release $a
    if (-not (Test-Path $src)) { throw "missing artifact: $src" }
}

# ---- locales/ の Unity .meta を退避 ----
# 共有スクリプトは .meta を関知しないため、退避・復元は Unity 配置固有の処理として
# ここで行う。旧ファイルの残留を避けるためディレクトリごと作り直す。
$LocalesDst = Join-Path $Dest 'locales'
$MetaTmp = $null
if (Test-Path $LocalesDst) {
    $metas = Get-ChildItem -Path $LocalesDst -Filter '*.meta' -ErrorAction SilentlyContinue
    if ($metas) {
        $MetaTmp = Join-Path ([System.IO.Path]::GetTempPath()) "cef-unity-meta-$([System.Guid]::NewGuid())"
        New-Item -ItemType Directory -Path $MetaTmp -Force | Out-Null
        foreach ($m in $metas) {
            Copy-Item -Path $m.FullName -Destination $MetaTmp -Force
        }
    }
    Remove-Item -Path $LocalesDst -Recurse -Force
}

# ---- Rust 成果物 + CEF ランタイムを配置 (Viewer のビルドと共有) ----
& (Join-Path $ScriptDir 'copy-windows-runtime.ps1') -Destination $Dest
if ($LASTEXITCODE -ne 0) { throw "copy-windows-runtime.ps1 failed" }

# 共有スクリプトは CEF ランタイム未検出でも警告のみで抜けるため、Unity 配置では結果を検査する。
foreach ($required in @('cef_unity_rust.dll', 'cef-unity-server.exe', 'libcef.dll')) {
    if (-not (Test-Path (Join-Path $Dest $required))) {
        throw "deploy failed: $required was not copied to $Dest"
    }
}

# ---- 退避した .meta を復元 ----
if ($MetaTmp -and (Test-Path $MetaTmp)) {
    Get-ChildItem -Path $MetaTmp -Filter '*.meta' | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $LocalesDst -Force
    }
    Remove-Item -Path $MetaTmp -Recurse -Force
}

Write-Host "[deploy] done -> $Dest"
