# CLAUDE.md

## ビルド・テスト・デプロイ

Rust 側のコードに変更を加えた場合、以下を必ず実行すること:

### 1. ビルドとテスト

```bash
cargo build
cargo test -p cef-unity-ipc
```

### 2. C# 側の同期更新

FFI 関数の追加・変更時は **`cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs` + `CefUnity.cs`** を更新する (namespace は `CefUnity` / `CefUnity.Interop`)。

Interop は逆転設計で **CefUnity.Core に一本化済み** — Unity・Harness・Tests はこの Core を単一の真実源として参照する (Unity 側 `Assets/CefUnity/Interop/` との二重管理は廃止)。

### 3. Unity プロジェクトへのデプロイ

#### macOS

`deploy.sh` を使う。ビルド・コピー・codesign を一括で行う:

```bash
bash deploy.sh
```

成果物は `cef-unity-unityproject/Assets/CefUnity/Plugins/osx-arm64/cef-unity-server.app` に配置される。

#### Windows (x86_64)

`deploy.ps1` を使う。MSVC link.exe / cl.exe にパスが通っている必要があるため、
Visual Studio Build Tools 2022 がある場合は VS Developer PowerShell から、
そうでなければ事前に `vcvars64.bat` を実行する:

```powershell
# Developer PowerShell for VS 2022 から:
.\deploy.ps1

# または通常の PowerShell から:
& "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
.\deploy.ps1
```

成果物は `cef-unity-unityproject/Assets/CefUnity/Plugins/win-x64/` にフラット配置される
(`cef_unity_rust.dll`, `cef-unity-server.exe`, `cef-unity-rust-helper.exe`, `libcef.dll`, 各種 `.pak` / `.dat` / `.bin`, `locales/`)。

Windows のゼロコピー GPU 経路は **D3D11 共有テクスチャ + 共有 fence** で実装済み
(macOS の IOSurface/Mach/Metal に相当)。server が `on_accelerated_paint` で共有 NT HANDLE を
publish し、client が `OpenSharedResource1` で開いて `ID3D11DeviceContext4::Wait` で同期する。

client が使う D3D11 device の取得元は 2 系統ある:

- **Unity**: `UnityPluginLoad` が `IUnityGraphicsD3D11` から取得する (呼び出し不要)
- **Unity 以外のホスト** (`CefUnity.Viewer` 等): `cef_unity_set_external_d3d11_device` で自前の
  `ID3D11Device` を注入する。**`cef_unity_create_browser` より前に呼ぶこと** (browser 生成時に
  共有 fence を開く判定が走るため)。device の所有権は呼び出し側にあり、client は AddRef せず
  借用するだけなので `cef_unity_shutdown` まで生存させること

#### Linux (x86_64)

フェーズ 1 時点では **Unity への deploy スクリプトは無い**。Rust 単体と C# Harness までの対応。

必要な apt パッケージ:

```bash
# ビルド
sudo apt-get install -y build-essential pkg-config curl git cmake python3
# CEF 実行時
sudo apt-get install -y \
  libnss3 libnspr4 libasound2t64 libatk1.0-0t64 libatk-bridge2.0-0t64 \
  libcups2t64 libdrm2 libgbm1 libgtk-3-0t64 libxcomposite1 libxdamage1 \
  libxfixes3 libxkbcommon0 libxrandr2 libpango-1.0-0 libcairo2 libx11-xcb1 libxss1
```

Harness の実行:

```bash
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
./CefUnity.Harness smoke          # フレームが取れるか
./CefUnity.Harness dump out.png   # 1 フレームを PNG 保存
```

`build-server-sandbox.sh` が Linux ではフラット配置を行う (macOS の .app バンドルに相当)。
`libcef.so` は `RPATH=$ORIGIN` で解決するため `LD_LIBRARY_PATH` の設定は不要。

apphost がランタイムを見つけられない場合は `export DOTNET_ROOT="$HOME/.dotnet"` を設定すること。

既知の制約:

- **software paint 経路のみ**。GPU ゼロコピー (dmabuf/EGL) は未実装
- ネイティブ音声出力 (macOS の AudioUnit 経路に相当) は無い。Unity ミキサ経路のみ
- Unity Editor / Player 対応は未着手
- フェーズ 1 の実測では command line switch の追加は不要だった (`SMOKE_OK` を確認済み)。
  ただし検証環境は WSLg (X11 が見える状態) であり、**X11 が無い真のヘッドレス環境は未検証**。
  その場合は `--ozone-platform=headless` の追加が必要になる可能性がある

## サポート対象プラットフォーム

| プラットフォーム | 状態 |
|---|---|
| macOS arm64 | GPU ゼロコピー (IOSurface/Mach/Metal) |
| Windows x64 | GPU ゼロコピー (D3D11 共有テクスチャ + 共有 fence) |
| Linux x86_64 | software paint のみ。Rust + Harness まで (Unity 未対応) |
| macOS x86_64 (Intel Mac) | **サポートしない** |

`deploy.sh` の配置先が `osx-arm64` にハードコードされているのは Intel Mac 非サポートの
方針によるもので、意図的なもの。

**注意:** Rust 側の変更が完了したら、必ず `deploy.sh` (macOS) または `deploy.ps1` (Windows) を実行すること。これを忘れると Unity プロジェクトに古いバイナリが残る。
