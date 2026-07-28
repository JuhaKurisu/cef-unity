# CefUnity.Viewer Windows 対応 — D3D11 表示経路 設計 (2026-07-28)

## 目的

`CefUnity.Viewer` を Windows で動かし、macOS と同じ GPU ゼロコピー経路 (accelerated paint) で
ページを表示・操作できるようにする。これにより「スクロールカクツキが Unity 固有か Core/CEF 側か」の
切り分けを Windows でも行えるようにする (Viewer 本来の目的、`2026-07-25-silknet-viewer-design.md` §目的)。

## スコープ

- 対象: `CefUnity.Viewer` の Windows x64 表示経路 (D3D11)、および Windows でビルド・実行するための
  周辺整備 (Harness の csproj ガード、ランタイム配置、CI、ドキュメント)
- 表示経路: **GPU ゼロコピー (accelerated paint / D3D11 共有テクスチャ / 共有 fence)**。`useGpu:true`
- **対象外**: Windows のネイティブ高精度スクロールソース (WndProc / Raw Input)。Windows では
  `ScrollInputPipeline.StartNativeSource` が `NotSupported` を返し、SDL の wheel イベントに
  フォールバックする現行挙動のままとする
- **対象外**: D3D12 経路。Viewer は D3D11 のみを使う (`cef_unity_is_d3d12_connected()` は false のまま)
- **対象外**: Unity プロジェクト側の Windows 動作確認。Plugins へのバイナリ配置は publish CI の責務であり、
  ローカルビルド成果物はコミットしない
- macOS の既存挙動は変更しない

## 前提として確認済みの事実

| 事実 | 根拠 |
|---|---|
| server の Windows accelerated paint は完全に配線済み (D3D11Pool / 共有 NT HANDLE / 共有 fence) | `crates/server/src/server.rs:350-410`, `crates/server/src/d3d11_pool.rs` |
| client は Unity の `IUnityGraphicsD3D11` からしか device を取得できない | `crates/client/src/d3d11.rs:142-168` |
| `open_fence` は `cef_unity_create_browser` の内部で `is_connected()` を条件に呼ばれる | `crates/client/src/lib.rs:586-601` |
| 共有テクスチャは `B8G8R8A8_UNORM_SRGB` (RGBA 時は `R8G8B8A8_UNORM_SRGB`) | `crates/server/src/server.rs:377-384` |
| server は既定アダプタ + `D3D_DRIVER_TYPE_HARDWARE` + `BGRA_SUPPORT` で device を作る | `crates/server/src/d3d11_pool.rs:127-` |
| `cef-unity-server.exe` は `cef_unity_rust.dll` と同じディレクトリ直下から起動される | `crates/client/src/lib.rs:114-117` |
| Windows のランタイム配置 (フラット) は `deploy.ps1` が定義済み | `cef-unity-rust/deploy.ps1` |
| Windows でも既存 C# テスト 101 件は通る (2026-07-28 ローカル実測) | `dotnet test cef-unity-csharp/CefUnity.Tests -c Release` |

## アーキテクチャ

### 起動順序 (設計の要)

`open_fence` は `cef_unity_create_browser` の内部で呼ばれるため、**デバイス注入が Browser 生成より
後だと GPU 同期 fence が張られない**。Windows では `Program` の初期化順序を次のとおりにする。

1. `D3D11GraphicsDevice` を生成 (`D3D11CreateDevice`: 既定アダプタ / `D3D_DRIVER_TYPE_HARDWARE` /
   `D3D11_CREATE_DEVICE_BGRA_SUPPORT`。server と同一条件にしてアダプタを揃える)
2. `CefRuntime.SetExternalD3D11Device(device)` で注入
3. `CefFrameSource` (= `Browser`) 生成 → ここで fence が開く
4. ウィンドウ生成 → `Load` で `D3D11FrameRenderer.Initialize(view)` が HWND を取りスワップチェーンを作る

macOS は現行順序のまま (デバイス注入なし)。

### 破棄順序

`Browser` (CEF shutdown) → `IFrameRenderer` → `D3D11GraphicsDevice` の順。client は注入デバイスを
借用するだけ (`from_raw_borrowed`、AddRef しない) なので、CEF 側が使い終わる前にデバイスを解放しては
ならない。

### コンポーネント

| ユニット | 責務 | 新規/変更 |
|---|---|---|
| `D3D11GraphicsDevice` | `ID3D11Device` / immediate `ID3D11DeviceContext` の生成と所有のみ。ウィンドウに依存しない | 新規 |
| `D3D11FrameRenderer` | HWND からスワップチェーンを作り、受信テクスチャをバックバッファへコピーして Present | 新規 |
| `FrameRendererFactory` | OS 判定でレンダラを選ぶ。判定は引数で差し替え可能にしてテストする | 新規 |
| `ViewerWindow` | `new MetalFrameRenderer(_sdl)` の直書きをやめ、`IFrameRenderer` をコンストラクタ注入で受け取る | 変更 |
| `CefFrameSource` | `TickFrame` の受信をプラットフォーム分岐 (mac: IOSurface + `ReleaseMetalTexture` / Windows: `TryReceiveD3D11Texture`、解放不要) | 変更 |
| `Program` | Windows のみ手順 1-2 を実行し、レンダラを factory から得て `ViewerWindow` へ渡す | 変更 |
| `IFrameRenderer` | 変更なし (`Initialize` / `Present` / `Dispose` のまま) | — |

プラットフォーム分岐は `FrameRendererFactory`・`CefFrameSource`・`Program` の 3 箇所だけに封じ込める
(`ScrollInputPipeline.StartNativeSource` と同じ流儀。呼び出し側に `#if` を増やさない)。

## D3D11FrameRenderer の詳細

### Initialize(IView)

`view.Native.Win32.Value.Hwnd` を取り、`IDXGIFactory2.CreateSwapChainForHwnd` でスワップチェーンを作る。

- `Format = B8G8R8A8_UNORM` (受信 format が RGBA のときは `R8G8B8A8_UNORM`。下記「format 追従」参照)
- `BufferCount = 2`、`SwapEffect = FLIP_DISCARD`、`Usage = RenderTargetOutput`、`SampleDesc = {1, 0}`

### Present(texturePointer, width, height)

1. `texturePointer == IntPtr.Zero` (起動直後): コピーせず `Present` だけ回す (現行の黒画面挙動と同じ)
2. **サイズ・format 追従**: バックバッファの寸法または format が受信テクスチャと一致しなければ
   `ResizeBuffers` (format 変更時は format 込みで再作成) し、**そのフレームは skip** して次フレームで
   コピーする。Metal 側の drawableSize 収束と同じ方針・同じ理由 (in-flight のバッファが旧サイズを
   持ちうるため)
3. 一致していれば `GetBuffer(0)` → `CopySubresourceRegion` (幅・高さは min クランプ) → `Present(1, 0)`

`Present(1, 0)` の vsync 待ちが mac の `displaySyncEnabled` 相当のフレームペーシングを担う
(`WindowOptions.FramesPerSecond = 0` のまま、タイマー駆動はしない)。

### 色と format

server はプールテクスチャを `B8G8R8A8_UNORM_SRGB` (RGBA 時は `R8G8B8A8_UNORM_SRGB`) で作る。
UNORM ↔ UNORM_SRGB は同 family なのでコピーは通り、生バイトがそのままバックバッファに入る。CEF の
出すバイトは sRGB エンコード済みなので、`_UNORM` のスワップチェーンで無変換表示すると正しく見える
(Metal 側の BGRA8Unorm blit と同じ理屈)。

ただし **RGBA と BGRA は family が異なりコピーが失敗する**ため、`TryReceiveD3D11Texture` が返す
`format` タグ (0=BGRA, 1=RGBA) を保持し、変化したらスワップチェーンを対応する family で作り直す。
CEF Windows OSR は通常 BGRA を出すので RGBA は稀なフォールバック経路だが、サイズ追従と同じ仕組みに
乗せられるため実装コストは小さい。

### 同期

`wait_fence` は client 内にキャッシュした *注入デバイスの immediate context* に `Wait` を積む
(`crates/client/src/d3d11.rs:230-249`)。したがってレンダラも `GetImmediateContext` で得た **同一
context** でコピーしなければ「server の書き込み完了 → Viewer の読み出し」の順序保証が壊れる。
deferred context は作らない。

### 解放

受信テクスチャは client 側がキャッシュ AddRef 管理するため Viewer では Release しない
(`NativeMethods.g.cs:395-396` の契約)。`Dispose` ではバックバッファ → スワップチェーン → factory の順に
解放し、デバイスは `D3D11GraphicsDevice` 側が持つ。

## Rust FFI 追加

```rust
// crates/client/src/d3d11.rs
pub fn set_external_device(device: *mut c_void)   // UNITY_DEVICE へ直接 store

// crates/client/src/lib.rs
#[unsafe(no_mangle)]  // edition 2024
pub extern "C" fn cef_unity_set_external_d3d11_device(device: *mut c_void)  // 非 Windows は no-op
```

doc コメントに次を明記する (csbindgen 経由で C# 側にも出る):

- **`cef_unity_create_browser` より前に呼ぶこと** (さもないと共有 fence が開かれない)
- デバイスの所有権は呼び出し側。client は AddRef せず借用するだけなので、CEF shutdown まで生存させること

C# 側は `Interop/CefUnity.cs` に `CefRuntime.SetExternalD3D11Device(IntPtr)` の static ラッパを足す
(`UNITY_DEVICE` はプロセスグローバルなので static が正しく、`Browser` のインスタンスメソッドにはしない)。

手順は `cef-unity-rust/CLAUDE.md` に従う: `cargo build --release` → csbindgen が
`cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs` を再生成 → 必要に応じて `deploy.ps1`。

## ビルド・配置・CI

### Harness の csproj ガード

`CefUnity.Harness.csproj` の `<None Include=".../libcef_unity_rust.dylib">` と `CopyServerApp`
ターゲットに、Viewer と同じ `IsOSPlatform('OSX') And Exists(...)` ガードを付ける。現状 Windows では
`MSB3030` でビルドが失敗する。

### Windows ランタイム配置

`cef-unity-server.exe` は `cef_unity_rust.dll` と同じディレクトリ直下に必要なので、Viewer の
`$(OutputPath)` へ Rust 成果物と CEF ランタイムをフラット配置する。

`deploy.ps1` の収集ロジック (Rust 成果物 3 種 + ランタイム DLL 群 + リソース + `locales/`) を
`copy-windows-runtime.ps1 <destination>` として切り出し、`deploy.ps1` は Unity の `win-x64` パスで、
`CefUnity.Viewer.csproj` の Windows 条件付きターゲットは `$(OutputPath)` で呼ぶ。mac が
`build-server-sandbox.sh` を deploy と Viewer で共有しているのと同じ構図にする。

`deploy.ps1` 固有の処理 (Unity `.meta` の退避・復元) は切り出し先ではなく `deploy.ps1` 側に残す。
コピー先が存在しない・CEF ランタイムが見つからない場合、Viewer のビルドは失敗させずスキップして警告に
留める (Rust 未ビルド環境で `dotnet build` が壊れないようにするため。Viewer 側の `Exists` ガードと同じ理由)。

### build-csharp.sh

`dotnet build` に `-p:Platform=AnyCPU` を明示し、`bin/Release/netstandard2.1/` へ出力先を固定する。
環境変数 `Platform=x64` が設定されたマシン (Visual Studio 環境) では出力が `bin/x64/Release/` になり、
現状の決め打ちパスでは `cp` が失敗する。

### CI

`.github/workflows/rust-build.yml` の `build-win` ジョブに `actions/setup-dotnet` (10.0.x) と
`dotnet test cef-unity-csharp/CefUnity.Tests -c Release` を追加する (`build-mac` ジョブと同じ形)。
これで C# の Windows 回帰が CI に入る。

### ドキュメント

- `cef-unity-rust/CLAUDE.md`: 「Windows ではゼロコピー GPU 経路は無効で software paint で動作する。
  将来的な D3D11 共有テクスチャ対応はフェーズ 2 で実装予定」は現状と食い違うため更新する
- `CefUnity.Viewer/README.md`: 冒頭の「mac 単体ブラウザ」を改め、Windows の実行手順と
  トラブルシュート (`taskkill /IM cef-unity-server.exe`、キャッシュは `%TEMP%` 配下) を追記
- `2026-07-25-silknet-viewer-design.md` のスコープ節「Windows は保留」に、本設計書への参照を追記

## エラー処理

| 事象 | 挙動 |
|---|---|
| `D3D11CreateDevice` 失敗 | HRESULT を添えてコンソールへ出力し非 0 終了 |
| 注入後も `IsD3D11Connected()` が false | 「デバイス注入に失敗」と原因候補を出力し非 0 終了 |
| スワップチェーン作成失敗 | HRESULT を添えて出力し非 0 終了 |
| テクスチャ受信が null | 前フレームを維持 (現行 mac と同じ)。起動直後は黒画面 |
| `CopySubresourceRegion` の family 不一致 | 起こらない設計 (format 追従で吸収)。万一起きた場合は 1 度だけ警告し、そのフレームを skip |

黙って黒画面のまま動き続ける経路は作らない。

## テストと検証

### 自動テスト

- 既存 101 テストが Windows で通り続けること (CI の `build-win` ジョブで担保)
- 新規ユニットテストは `FrameRendererFactory` の選択ロジックのみ (OS 判定を引数に切り出す)
- D3D11 の実呼び出しは実機・GPU 依存のためユニットテストしない (Metal 側も同様で、既存流儀に合わせる)

### 実機検証 (完了判定)

1. `dotnet run --project cef-unity-csharp/CefUnity.Viewer -- spike` — SDL 窓 + D3D11 デバイス +
   スワップチェーン + テクスチャ 1 枚受信の疎通 (現行 spike の mac 専用処理は Windows では該当分を差し替え)
2. `--url <url>` でページが表示されることをスクリーンショットで確認
3. マウス移動・クリック・ホイールスクロール・キーボード入力・ウィンドウリサイズが動作すること
4. ウィンドウを閉じた後に `cef-unity-server.exe` が残留しないこと (`tasklist` で確認)

## 実装順序

1. Harness の csproj ガード + `build-csharp.sh` 修正 (Windows でビルドが通る状態にする)
2. Rust FFI 追加 → `cargo build --release` → `NativeMethods.g.cs` 再生成 → C# ラッパ
3. `copy-windows-runtime.ps1` 切り出し + Viewer csproj の配置ターゲット
4. `D3D11GraphicsDevice` / `D3D11FrameRenderer` / `FrameRendererFactory` 実装と配線変更
5. 実機検証 (spike → 表示 → 入力)
6. CI とドキュメント更新
