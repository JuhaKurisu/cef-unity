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

**注意:** Rust 側の変更が完了したら、必ず `deploy.sh` (macOS) または `deploy.ps1` (Windows) を実行すること。これを忘れると Unity プロジェクトに古いバイナリが残る。
