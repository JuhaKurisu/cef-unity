# CLAUDE.md

## ビルド・テスト・デプロイ

Rust 側のコードに変更を加えた場合、以下を必ず実行すること:

### 1. ビルドとテスト

```bash
cargo build
cargo test -p cef-unity-ipc
```

### 2. C# 側の同期更新

FFI 関数の追加・変更時は以下の **両方** を更新する:

- **cef-unity-csharp** (Sandbox 用): `cef-unity-csharp/Interop/NativeMethods.g.cs` + `CefUnity.cs`
- **cef-unity-unityproject** (Unity 用): `cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs` + `CefUnity.cs`

両方のファイルは同じ内容を維持すること (namespace の違いに注意: csharp 側は `Interop`、Unity 側は `CefUnity.Interop`)。

### 3. Unity プロジェクトへのデプロイ

#### macOS

`deploy.sh` を使う。ビルド・コピー・codesign を一括で行う:

```bash
bash deploy.sh
```

成果物は `cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/osx-arm64/cef-unity-server.app` に配置される。

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

成果物は `cef-unity-unityproject/Assets/CefUnity/Interop/Plugins/win-x64/` にフラット配置される
(`cef_unity_rust.dll`, `cef-unity-server.exe`, `cef-unity-rust-helper.exe`, `libcef.dll`, 各種 `.pak` / `.dat` / `.bin`, `locales/`)。

Windows ではゼロコピー GPU 経路 (IOSurface/Mach/Metal) は無効で、software paint (共有メモリ経由の BGRA 転送) で動作する。
将来的な D3D11 共有テクスチャ対応はフェーズ 2 で実装予定。

**注意:** Rust 側の変更が完了したら、必ず `deploy.sh` (macOS) または `deploy.ps1` (Windows) を実行すること。これを忘れると Unity プロジェクトに古いバイナリが残る。
