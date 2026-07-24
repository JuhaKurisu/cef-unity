# CefUnity.Viewer — Silk.NET 単体ブラウザ 設計 (2026-07-25)

## 目的

1. **スクロールカクツキの切り分け**: Unity と同一の入力パイプライン (Core の ScrollInputPipeline) を Unity 以外のホストで動かし、カクツキが再現するかで「Unity 固有か / Core・CEF 側か」を判定する
2. **軽量ホスト**: Unity より軽い単体ブラウザとして表示+操作 (マウス/スクロール/キーボード/IME) ができること

## スコープ

- **対象 OS: macOS のみ** (Windows は保留。将来 D3D11 対応する際は `IFrameRenderer` 抽象に `D3D11FrameRenderer` を追加し、native 側にデバイス注入 FFI `cef_unity_set_external_d3d11_device` を 1 本追加する — 現 native は UnityPluginLoad 経由でしか D3D11 デバイスを取得できないため)
- 表示経路: **GPU ゼロコピー (accelerated paint / IOSurface / Metal)**。useGpu:true で初期化
- 入力: マウス (移動/クリック/ホイール)、キーボード、**IME (日本語入力)**、ウィンドウリサイズ追従
- スクロール検証: **フルマトリクス (3 モード実行時切替) + 録画/リプレイ内蔵**
- URL バー等の UI は作らない。URL は CLI 引数指定
- **Rust 変更ゼロ** (mac は既存 FFI だけで完結)

## アーキテクチャ

新プロジェクト `core/CefUnity.Viewer` (net10.0 exe)。参照は `CefUnity.Core` のみ。Harness と同様、ビルド時に dylib と server.app バンドルを出力先へコピー (`build-server-sandbox.sh` 再利用)。

### コンポーネント (各 1 ファイル・単一責務)

| ユニット | 責務 |
|---|---|
| `Program` | CLI 解析 (`--url` `--size` `--scroll-mode` `--record` `--replay <csv>` `--statistics <csv>`)、全体の配線 |
| `ViewerWindow` | Silk.NET.Windowing **SDL バックエンド** (GraphicsAPI.None)、イベント配線、`SDL_AddEventWatch` で IME (TEXTEDITING/TEXTINPUT)・精密ホイールの生イベント取得 |
| `IFrameRenderer` | `Initialize / Present(textureHandle, width, height) / Resize / Dispose` の表示抽象 (将来の D3D11 追加の継ぎ目) |
| `MetalFrameRenderer` | `SDL_Metal_CreateView` → CAMetalLayer 取得、objc_msgSend P/Invoke で blit+present。**毎フレーム objc_autoreleasePoolPush/Pop で囲む** (Rust スレッドの autorelease 蓄積と同種の既知の罠対策) |
| `CefFrameSource` | `SendExternalBeginFrame` + `Pump` + `cef_unity_receive_iosurface_texture` によるテクスチャ受信 + `Resize`+invalidate の CEF 側窓口 |
| `ScrollInputMatrix` | スクロール 3 モードの実行時切替 (F1/F2/F3)、録画トグル (F5)、リプレイ実行。Core の ScrollInputPipeline / MacNativeScrollSource / ScrollSmoother / ScrollResampler / ScrollReplayRunner をそのまま使用 |
| `SilkKeyboardMapper` | SDL キーコード → windowsKeyCode/CefKeyCodes 変換 (純ロジック。Unity 用 CefKeyboardMapper は UnityEngine.KeyCode 依存のため流用不可、新規実装) |
| `ImeBridge` | SDL TEXTEDITING/TEXTINPUT ⇔ CEF ImeSetComposition/ImeCommitText/ImeFinishComposingText の橋渡し、SHM キャレット位置 → `SDL_SetTextInputRect` |
| `StatisticsRecorder` | フレーム dt・paint fps (accelerated_frame_id 差分)・sentDy (送信スクロール量) の CSV 出力、粗さ指標の算出 (Unity 側 C案検証で用いた指標と同一定義を実装計画で特定し流用する) |

### 選定理由

- **SDL バックエンド** (GLFW ではなく): SDL2 は IME をクロスプラットフォーム標準サポート (TEXTEDITING=変換中 preedit / TEXTINPUT=確定 / SDL_SetTextInputRect=候補窓位置)。GLFW は preedit 非対応でネイティブフックが必要になる。さらに `SDL_Metal_CreateView` で CAMetalLayer が直接取れるため表示側とも相性が良い
- **描画層は C# で実装** (native FFI 追加ではなく): `cef_unity_receive_iosurface_texture` はシステム既定 Metal デバイスで MTLTexture を返し、Apple Silicon では CAMetalLayer 側と同一デバイスになるため C# からの blit で完結する。Rust/deploy 手順が不要

## データフロー

### フレームループ (vsync 駆動 ≈60Hz)

CAMetalLayer の displaySyncEnabled (既定 true) でハードウェア同期。Unity の Thread.Sleep/targetFrameRate 方式より正確な 60Hz。

1. スクロールパイプライン tick (`TickResampler`/`TickSmoother`) → `Drain(overBrowser, scale)` → `SendMouseWheel`
2. マウス/キーボード/IME イベント送信 (SDL イベントから)
3. `SendExternalBeginFrame(frameIndex)` → `Pump()`
4. `cef_unity_receive_iosurface_texture` で MTLTexture 受信 (null なら前フレーム維持)
5. autoreleasepool 内で: nextDrawable → blit (受信テクスチャ → drawable.texture) → present → commit

### スクロールマトリクス (F1/F2/F3 切替、現在モードをウィンドウタイトルに表示)

| モード | ソース | パイプライン | 対応する Unity 経路 |
|---|---|---|---|
| ①raw | SDL wheel イベント | なし (1:1 直送) | 平滑化前の旧 Unity |
| ②smoother | SDL wheel イベント | ScrollSmoother | Unity A案 (レガシートグル) |
| ③resampler | MacNativeScrollSource (NSEvent monitor) | ScrollResampler 予測モード | Unity C案 (現行既定) |

- 録画: Core の `RecordingEnabled` トグル ($TMPDIR/cef_scroll_events.csv)
- リプレイ: `--replay <csv>` 起動で `ScrollReplayRunner` に配線
- **切り分けの実験プロトコル**: Unity で録画した CSV を Viewer でリプレイ → StatisticsRecorder の粗さ指標を Unity 実測値 (0.088) と比較。**再現すれば Core/CEF 側、再現しなければ Unity 側**と結論する

### IME

- TEXTEDITING → `ImeSetComposition` (変換中テキスト+下線)
- TEXTINPUT → 変換確定は `ImeCommitText`、非変換時 (ASCII 直接入力) は `SendCharEvent`
- フォーカス喪失/Esc → `ImeFinishComposingText` / `ImeCancelComposition`
- SHM 経由のキャレット位置 (CARET_TRACKING_JS が書き込み、Interop の `GetImeCaret` で取得) を毎フレーム読み、`SDL_SetTextInputRect` で候補窓を追従

### リサイズ

SDL リサイズイベント → `Browser.Resize` + invalidate (was_resized だけでは再描画されない既知の罠対応) → CAMetalLayer の drawableSize 更新。受信テクスチャとウィンドウのサイズ不一致中は旧フレームをスケール描画 (黒フレーム防止)。

## エラー処理

- CEF サーバー起動失敗 → コンソールにエラーと復旧手順 (キャッシュ破損時は `cef_unity_cache` 削除案内) を出して非 0 終了
- テクスチャ受信 null → 前フレーム維持 (起動直後は黒画面+ウィンドウタイトルに "loading")
- ウィンドウクローズ → Browser dispose (CEF shutdown) を必ず実行しサーバープロセス残留を防ぐ。異常終了時の復旧 (`pkill -f cef-unity-server`) は README に記載

## テスト

- **ユニット** (CefUnity.Tests に追加、Unity 不要で `dotnet test`): SilkKeyboardMapper の変換表 / ImeBridge の状態機械 (変換中・確定・キャンセル遷移) / ScrollInputMatrix のモード切替とパイプライン配線
- **リプレイ検証**: 既存 ScrollReplayRunner + 録画 CSV による自己検証 (Unity 側で確立済みの手法)
- **実験プロトコル** (本来の目的): 上記スクロールマトリクス節のとおり

## 実装前スパイク (失敗したら設計に立ち戻る)

- **S1**: MacNativeScrollSource (NSEvent monitor) が SDL イベントループ下で発火するか
- **S2**: Silk.NET.Windowing の SDL バックエンドから `Sdl` API インスタンスと `SDL_AddEventWatch` が使えるか (TEXTEDITING 取得の前提)
- **S3**: GraphicsAPI.None の SDL 窓で `SDL_Metal_CreateView` → CAMetalLayer が取れて、受信 MTLTexture の blit+present が動くか
- **S4**: SDL 窓が「真にアクティブ」として CEF paint を止めないこと (programmatic 注入時に paint が凍結する既知の罠)

## 決定履歴

- 切り分け構成: フルマトリクス+リプレイ内蔵 (ユーザー選択)
- 表示経路: Metal + IOSurface の GPU ゼロコピー (ユーザー選択。CPU/OpenGL 案は不採用)
- Windows D3D11: 一度スコープ入りしたが**保留に変更** (ユーザー判断)。IFrameRenderer 抽象と native デバイス注入 FFI の必要性のみ本 spec に記録
- 初期スコープ: キーボード+リサイズ+IME 込み、URL バーなし (ユーザー選択)
- 窓バックエンド: Silk.NET + SDL (ユーザー承認。GLFW+ネイティブ IME フック案は不採用)
- 描画層: C# 実装 (ユーザー選択。native FFI 追加案は不採用)
