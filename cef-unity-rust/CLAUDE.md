# CLAUDE.md

## ビルド・テスト・デプロイ

Rust 側のコードに変更を加えた場合、以下を必ず実行すること:

### 1. ビルドとテスト

```bash
cargo build
cargo test -p cef-unity-ipc
```

Windows では MSVC 環境が要る (下記「落とし穴」参照):

```powershell
cmd /c '"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul && cargo test'
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

**落とし穴:** Git Bash から `cargo` を直接叩くと、`/usr/bin/link` が MSVC の `link.exe` を
隠してリンクエラーになる (`extra operand ... .rcgu.o`)。Windows では PowerShell から
`vcvars64.bat` 経由で実行すること。

##### GPU ゼロコピー経路 (D3D11 / D3D12)

server は `on_accelerated_paint` で CEF の共有テクスチャを自前の出力テクスチャへ
`CopyResource` し、その NT 共有 HANDLE と **共有 fence** の値を shm に publish する
(`d3d11_pool.rs`)。KeyedMutex は使わない (D3D12 リソースからは `IDXGIKeyedMutex` を
取得できず、helper device 経由だと implicit fence が cross-device に効かないため撤去済み)。

client は Unity の graphics backend に応じて 2 経路を使い分ける。`UnityPluginLoad` が
D3D11/D3D12 の両方を試し、生きている方が使われる:

| backend | テクスチャを開く | 同期 |
|---|---|---|
| D3D11 (`d3d11.rs`) | `OpenSharedResource1` | `ID3D11DeviceContext4::Wait` |
| D3D12 (`d3d12.rs`) | `ID3D12Device::OpenSharedHandle` | `ID3D12CommandQueue::Wait` |

**Unity は既定で D3D12 を選ぶ** (`ProjectSettings` の `m_APIs` が D3D12 優先) ため、
実際の主経路は D3D12 側になることが多い。D3D11 を強制して検証したい場合は
`m_APIs: 0200000012000000` / `m_Automatic: 0` に変更して Editor を再起動する。

**D3D11 経路の呼び出しスレッド規約:** `d3d11.rs` の `wait_fence` / `open_or_cached` は
Unity のメインスレッドからのみ呼ぶこと。`ID3D11DeviceContext` は非スレッドセーフで、
Unity の render thread と競合し得る (D3D12 の `ID3D12CommandQueue::Wait` はスレッドセーフ
なのでこの制約は無い)。違反を検出できるよう呼び出しスレッド ID を記録しており、
変化すると `%TEMP%\cef_unity_debug.log` に WARNING が出る。

client が使う D3D11 device の取得元は 2 系統ある:

- **Unity**: `UnityPluginLoad` が `IUnityGraphicsD3D11` から取得する (呼び出し不要)
- **Unity 以外のホスト** (`CefUnity.Viewer` 等): `cef_unity_set_external_d3d11_device` で自前の
  `ID3D11Device` を注入する。**`cef_unity_create_browser` より前に呼ぶこと** (browser 生成時に
  共有 fence を開く判定が走るため)。device の所有権は呼び出し側にあり、client は AddRef せず
  借用するだけなので `cef_unity_shutdown` まで生存させること

##### プレイヤービルド

`CefBuildPostProcessor.PostProcessWindows` が `win-x64/` の中身を
`<App>_Data/Plugins/x86_64/` へ再帰コピーする (`cef_unity_rust.dll` は Unity が自動配置
するので除外、`.meta` はスキップ)。`CefUnity/Build Windows Player (measure)` メニュー
または `-executeMethod CefUnity.Editor.CefQuickBuild.BuildWindows` でビルドできる。

既知の制限:

- **ARM64 プレイヤービルドは未対応**。`PostProcessWindows` はコピー元 `win-x64`・
  コピー先 `Plugins/x86_64` が決め打ちで、`win-arm64` を見ない
- `win-arm64` はクロスビルドのみで実行検証をしていない (CI も x64 ランナー上でビルドするだけ)

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
| Windows x64 | GPU ゼロコピー (D3D11/D3D12 共有テクスチャ + 共有 fence)。Editor / プレイヤーとも動作確認済み |
| Windows arm64 | クロスビルドのみ。**実行未検証**、プレイヤービルド未対応 |
| Linux x86_64 | software paint のみ。Rust + Harness まで (Unity 未対応) |
| macOS x86_64 (Intel Mac) | **サポートしない** |

### プラットフォーム別の機能対応

| 機能 | macOS | Windows | Linux |
|---|---|---|---|
| GPU ゼロコピー | IOSurface/Metal | D3D11 / D3D12 | 未実装 (software) |
| ネイティブ音声出力 | AudioUnit | WASAPI | 無し (Unity ミキサのみ) |
| 生スクロール入力 | NSEvent モニタ | Raw Input | 無し (frame-polled) |
| キーリピート設定 | NSEvent の OS 値 | `SystemParametersInfo` の OS 値 | 既定値固定 |

ネイティブ音声出力と生スクロール入力は、出力/入力デバイス層だけがプラットフォーム依存で、
共通ロジックは 1 箇所に集約してある:

- 音声: SHM ドレイン + steering リングは `audio_pull.rs`。macOS は `native_voice.rs`
  (AudioUnit)、Windows は `wasapi_output.rs` (WASAPI) が出力層
- スクロール: 分岐は `ScrollInputPipeline.StartNativeSource` の 1 箇所のみ。
  呼び出し側に `#if` を増やさないこと

`deploy.sh` の配置先が `osx-arm64` にハードコードされているのは Intel Mac 非サポートの
方針によるもので、意図的なもの。

**注意:** Rust 側の変更が完了したら、必ず `deploy.sh` (macOS) または `deploy.ps1` (Windows) を実行すること。これを忘れると Unity プロジェクトに古いバイナリが残る。
