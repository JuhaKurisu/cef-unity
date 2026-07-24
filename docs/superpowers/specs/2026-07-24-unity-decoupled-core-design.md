# 逆転設計: 純 .NET コア + Unity は DLL 消費(テスト/実機ハーネスを Unity 無しで回す)

- 日付: 2026-07-24
- ステータス: ドラフト(ユーザーレビュー待ち)
- 対象リポジトリ: `cef-unity`

## 1. 目的と背景

これまでスクロール/音声/ペーサ等のロジック検証は Unity(Editor Test Runner / batchmode)を起動して行っていた。狙いは **これらのテストと「実機 CEF を回す挙動検証」を Unity 無しで実行できるようにする** こと。

調査の結果、テスト対象ロジック(`ScrollSmoother` / `ScrollResampler` / `ScrollInputPipeline` / `ScrollInputEvent` / `CefZeroFramePacer` / `CefAudioRing` / `MacNativeScrollSource`)は **既に UnityEngine 非依存の純 C#** で、時間 `dt` も `Tick(dt, tau, …)` のように引数注入されている。既存の NUnit テストは全 60 件で、すべてこの純ロジックを対象にしている。また純 C# の土台 `cef-unity-csharp/`(Interop + Sandbox)が既にあり、`CefRuntime.Init → Pump → TryGetBuffer → Shutdown` という Unity と同じ呼び出し方でヘッドレスに CEF を回せている。

したがって本設計は **アーキテクチャを「逆転」させる**: 純 .NET のコアを唯一の真実源(source of truth)として中心に据え、Unity を「1つのアダプタ・ホスト」に降格させ、Unity 固有機能(描画・音声・ログ・入力源)を外から注入する。Unity はコアを **ビルド済み DLL として消費** する。

## 2. アーキテクチャ(Ports & Adapters / ヘキサゴナル)

```
        ┌────────── CefUnity.Core (純 .NET, netstandard2.1, UnityEngine 禁止) ──────────┐
        │  Interop(P/Invoke 一本化)   ScrollSmoother / Resampler / Pipeline / Event    │
        │  CefZeroFramePacer   CefAudioRing   ScrollReplay(ロジック)                    │
        │  ── ポート(interface)──  IScrollEventSource(既存・移設)                        │
        └───────────▲───────────────────────────────────────────────▲──────────────────┘
                    │ 注入(adapter)                                  │ 注入(adapter)
   ┌────────────────┴──────────────────┐             ┌───────────────┴───────────────────┐
   │ Unity(UnityEngine 参照, Core.dll 消費)│             │ CefUnity.Harness(Console, net10)  │
   │  MonoBehaviour ホスト(旧 Sample)   │             │  ヘッドレス擬似 Unity ループ        │
   │  CefLog → Debug.Log(残置)          │             │  録画リプレイ源(IScrollEventSource) │
   │  CefAudioSink/Output/NativeAudio    │             │  PPM 出力 / スクリプト注入           │
   │  Texture2D アップロード              │             └───────────────┬───────────────────┘
   │  Unity Input → IScrollEventSource   │           ┌─────────────────┴───────────────────┐
   │  CefKeyboardMapper(UnityEngine.KeyCode)│         │ CefUnity.Tests(NUnit, net10, 60件)│
   └─────────────────────────────────────┘           └───────────────────────────────────┘
```

**設計原則**: コアは Unity の存在を知らない。純度は `asmdef` の `noEngineReferences: true` 相当を **コアには適用しない**(コアは Unity プロジェクト外の .NET ライブラリなのでそもそも UnityEngine を参照できない)。Unity 側で誤って UnityEngine 依存をコアへ持ち込むことは、コアが独立 .csproj であるため構造的に不可能。

**名前空間**: Core は Unity 側の既存名前空間 `CefUnity`(NativeMethods)/ `CefUnity.Interop`(Browser・CefRuntime)/ `CefUnity.Runtime`(スクロール・音声リング等)を**そのまま採用**する。これにより Unity 消費側の `using` 変更が最小化される(名前空間はアセンブリを跨げるので、`CefUnity.Runtime` が Core.dll と Unity asmdef に分かれても問題ない。型名の重複だけ避ける)。Harness/Tests は `using Interop;` を `using CefUnity.Interop;` に更新する。

## 3. リポジトリ構成(after)

```
cef-unity-rust/                          ← 変更なし(Rust: server / helper / dylib)
core/
  CefUnity.Core/CefUnity.Core.csproj     ← netstandard2.1・真実源・LangVersion latest
      Interop/  (CefRuntime, Browser, NativeMethods.g.cs, CefKeyCode, enums)  一本化
      Scroll/   (ScrollSmoother, CefZeroFramePacer)
      ScrollInput/ (ScrollInputEvent, ScrollResampler, ScrollInputPipeline,
                    IScrollEventSource, MacNativeScrollSource)
      Audio/    (CefAudioRing)
      Replay/   (ScrollReplay ロジック — Editor から移設)
  CefUnity.Harness/CefUnity.Harness.csproj  ← net10・擬似 Unity ループ・PPM・録画リプレイ
  CefUnity.Tests/CefUnity.Tests.csproj      ← net10・NUnit・60 テスト(dotnet test)
  CefUnity.sln
  build-core.sh                          ← Core を Release ビルド→DLL を Unity へ配置
cef-unity-unityproject/Assets/CefUnity/
  Plugins/CefUnity.Core.dll (+ .meta)    ← 消費物(LFS コミット)
  Plugins/osx-arm64/{libcef_unity_rust.dylib, cef-unity-server.app}   ← 案B で Interop から移設
  Plugins/win-x64/{libcef.dll, cef-unity-server.exe, …}               ← 案B で Interop から移設
  Runtime/  (Unity 残置: CefUnityBrowserSample[MonoBehaviour], CefLog,
             CefKeyboardMapper, CefAudioOutput/Sink/NativeAudio, Texture 転送)
  Editor/   (ScrollReplay.Run 薄ラッパ, CefQuickBuild, CefFpsMonitorWindow, CefBuildPostProcessor)
```

既存 `cef-unity-csharp/`(Interop + Sandbox)は `core/` に **吸収**する(Sandbox → Harness に発展)。dylib/CEF framework 同梱スクリプト(`build-server-sandbox.sh`, `CefUnity.targets`)は Harness 側で再利用する。

## 4. 移設マップ

| 移設元(現在) | 移設先 | 備考 |
|---|---|---|
| `Runtime/ScrollSmoother.cs`, `Runtime/CefZeroFramePacer.cs`, `Runtime/CefAudioRing.cs` | **Core** | ロジック無改変 |
| `Runtime/ScrollInput/*`(`IScrollEventSource` 含む) | **Core** | ロジック無改変 |
| `Interop/*`(Unity 側と `cef-unity-csharp/` 側の**二重を1本化**) | **Core** | `[DllImport("cef_unity_rust")]`。§6 の `#if` 対処を付帯 |
| `Editor/ScrollReplay.cs` の**ロジック部** | **Core/Replay** | Unity 側は batchmode `Run()` 薄ラッパのみ残置(`CefUnity.Core` を参照) |
| `Runtime/CefAudioRing.cs`(`using System` のみ) | **Core** | ロジック無改変。単体テスト対象 |
| `Runtime/CefLog.cs`(`Debug.Log` ラッパ) | **Unity 側に残置** | Core へ移す純ロジックは CefLog を呼ばない(呼ぶのは Unity 側4ファイルのみ)。logging ポート新設は不要 |
| `Runtime/CefKeyboardMapper.cs`(`UnityEngine.KeyCode` 使用) | **Unity 側に残置** | 本質的に Unity 入力アダプタ。Core は `CefKeyCode`/`CefKeyCodes` を公開 |
| `Runtime/CefUnityBrowserSample.cs`(MonoBehaviour), `CefAudioOutput.cs`, `CefAudioSink.cs`(MonoBehaviour), `CefNativeAudio.cs` | **Unity 側に残置** | Core.dll を参照して駆動 |
| **ネイティブ Plugins**(`Interop/Plugins/{osx-arm64,win-x64}`) | **`Assets/CefUnity/Plugins/` へ移設(案B)** | `.meta` ごと移動して GUID/プラットフォーム設定を保持 |

## 5. 注入ポート(= Unity 固有機能の差込口)

調査の結果、**新設する抽象はゼロ**。Core が実際に必要とする差込口は既存の `IScrollEventSource` 1本のみで、時間は既に引数注入されている。ログ・音声出力の抽象は Core 側に不要と判明した(下表)。

| 差込口 | 種別 | Unity アダプタ | Harness アダプタ |
|---|---|---|---|
| `IScrollEventSource` | 既存 interface(Core へ移設) | `MacNativeScrollSource` / Unity Input フォールバック | 録画/スクリプト源 |
| 時間 `dt` | 引数注入(既存 `Tick(dt,…)`) | `Time.deltaTime` を渡す | 固定 16.67ms or 実時計 |

**ログ**: Core へ移す純ロジック(ScrollSmoother/Resampler/Pipeline/Pacer/AudioRing)は `CefLog` を一切呼ばない(呼ぶのは Unity 側の CefNativeAudio/CefAudioOutput/CefAudioSink/CefUnityBrowserSample のみ)。よって `CefLog` は Unity 側にそのまま残し、Core に logging ポートは設けない。

**音声**: `CefAudioSink` は `MonoBehaviour`(AudioSource / `OnAudioFilterRead` 依存)であり interface ではない。Unity 側にそのまま残す。Core へ移すのは純粋な `CefAudioRing`(リングバッファ本体、`using System` のみ)だけで、これは sink に依存しない。よって `IAudioSink` 抽象は不要。

## 6. Interop の DLL 化(削除可能性の担保)

- Interop は **UnityEngine の型を一切使っていない**(`Texture2D` はドキュメントコメント内の言及のみ)。実体は `IntPtr` / `byte[]` / `ReadOnlySpan<byte>` を返す純 `[DllImport]` P/Invoke。
- `NativeMethods.g.cs` は **`[DllImport]`(旧来 P/Invoke)** を使用し、`[LibraryImport]`(ソースジェネレータ)ではない。生成済みファイルをコミットする方式でライブジェネレータ依存なし。→ **netstandard2.1 でビルド可能・ポリフィル不要**(`record`/`init`/`MathF` 不使用、`ReadOnlySpan<byte>` は netstandard2.1 に存在)。
- **マネージド DLL にネイティブを埋め込むわけではない**。`CefUnity.Core.dll`(マネージド)には P/Invoke 宣言(メタデータ)のみが入り、ネイティブ `libcef_unity_rust.dylib` / `cef_unity_rust.dll` は **別ファイルのまま**。実行時のネイティブ解決はライブラリ名 `cef_unity_rust` の文字列で行われ、どのアセンブリが宣言したかに依存しない。IL2CPP プレイヤービルドでも参照アセンブリ内の `[DllImport]` は動作する。
- **唯一の Unity プリプロセッサ依存**は `CefUnity.cs` の `IsAcceleratedConnected()`(現 538–547 行)の `#if UNITY_STANDALONE_OSX/WIN` 分岐のみ(Interop 全体で `#if` はこの1ブロックだけ)。素の .NET DLL では未定義になるため、**実行時 OS 判定に置換**する:

  ```csharp
  if (RuntimeInformation.IsOSPlatform(OSPlatform.OSX))     return IsIOSurfaceConnected();
  if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows)) return IsD3D11Connected() || IsD3D12Connected();
  return false;
  ```

  Unity・standalone 双方で同一挙動。この1メソッドの置換後、Interop は 100% エンジン非依存となり、Unity 側 `Assets/CefUnity/Interop/`(マネージドソース + asmdef)は削除できる。

## 7. ターゲットフレームワークと DLL 消費

- **Core**: `netstandard2.1`(Unity 6 と net10 の双方から参照可)、`LangVersion latest`、`AllowUnsafeBlocks true`、`Nullable enable`。
- **Harness / Tests**: `net10.0`(Core を参照)。
- Unity(6000.3.8f1)は `CefUnity.Core.dll` を **マネージドプラグイン**として `Assets/CefUnity/Plugins/` から自動参照・自動バンドルする。

## 8. ビルド/配布(build-core.sh)

- `build-core.sh`: `dotnet build core/CefUnity.Core -c Release` → 生成した `CefUnity.Core.dll` を `Assets/CefUnity/Plugins/` へコピー。
- `CefUnity.Core.csproj` の post-build ターゲットにも同コピーを仕込み、`dotnet build` 一発で Unity に反映されるようにする(Option B のトレードオフである反復コストを緩和)。
- **DLL は dylib と同様に LFS でコミット**する。Unity ユーザーが dotnet 無しでもプロジェクトを開ける状態を維持する。
- 反復ワークフロー: コア修正 → `build-core.sh`(DLL 再生成)→ Unity が再読込。

## 9. ビルド後処理(CefBuildPostProcessor)への影響

- `CefBuildPostProcessor` は **Interop の C# 型を参照していない**(`using Interop`/`CefRuntime`/`NativeMethods`/`new Browser` は 0 件)。`Application.dataPath` 起点のファイルコピーのみ。→ Interop マネージドソース/asmdef 削除で **コンパイルは壊れない**。
- Core.dll(マネージド)は Unity が自動バンドルするため **post-processor に追加処理は不要**。post-processor の責務(Unity が自動コピーしない server.app / Windows CEF ランタイムの手動配置)は不変。
- **案B に伴う唯一の編集**: ネイティブ Plugins を `Assets/CefUnity/Plugins/{osx-arm64,win-x64}/` へ移設するのに合わせ、`CefBuildPostProcessor` のソースパス文字列を **2箇所**更新する:
  - macOS: `Path.Combine(Application.dataPath, "CefUnity", "Interop", "Plugins", "osx-arm64", "cef-unity-server.app")` → `"CefUnity", "Plugins", "osx-arm64", …`
  - Windows: `Path.Combine(Application.dataPath, "CefUnity", "Interop", "Plugins", "win-x64")` → `"CefUnity", "Plugins", "win-x64"`
- ロジック(コピー処理・`callbackOrder`)の変更は不要。ネイティブ `.meta` はフォルダごと移動して GUID/プラットフォーム設定を保持する。

## 10. テストとハーネス

- **Tests**: 60 件を `core/CefUnity.Tests` に移し、`cd core && dotnet test` で Unity 無し・数秒で完走。Unity Test Runner 用の `Runtime.Tests.asmdef` は廃止する(「Unity 無しで回す」目的に合致し、二重管理を避ける)。
  - `ScrollDroughtRecordingTests` の `Application.dataPath` 依存は、録画フィクスチャの探索パスを Harness/Tests 側のパス解決に置換する(または録画パスをテストへ渡す)。
- **Harness**: `CefUnityBrowserSample` のフレームループの本質(`Init → Pump/SendExternalBeginFrame → スクロール注入を ScrollInputPipeline/Smoother/Resampler 経由 → SendMouseWheel → TryGetBuffer`)を MonoBehaviour 無しで純 C# ホストに再現。ページ読込→録画スクロールトレース注入→per-frame `sentDy` 記録→滑らかさ検証、を `dotnet run` で実行。ScrollReplay 自己検証が Unity batchmode 無しで回る。

## 11. CI

- 既存 `.github/workflows/rust-build.yml` に **`dotnet test`(Core/Tests)** を追加する。
- Core DLL の更新(`build-core.sh`)を publish 経路に組み込む(タグ/dispatch 時に DLL を再生成して LFS コミットに含める)。詳細は実装計画で詰める。

## 12. 主な結果・リスク

- Unity 側の実変更: `Assets/CefUnity/Interop/`(マネージド + asmdef)削除、Core.dll 参照へ切替、ネイティブ Plugins 移設、アダプタ整理、Editor 系の `using` 更新(`CefUnity.Runtime` → Core の名前空間、ScrollReplay 薄ラッパ化)。ユーザー承認済み。
- 反復コスト: コア修正のたびに `build-core.sh` による DLL 再生成が必要(Option B のトレードオフ)。ホットリロード/コアへのステップ実行は Unity から直接は効かない。
- Harness の P/Invoke・dylib 探索・CEF framework 同梱は既存 Sandbox で実証済みのため低リスク。
- IL2CPP プレイヤービルドでの Core.dll 内 `[DllImport]` 動作は標準機能だが、実ビルドで最終確認する。

## 13. テスト戦略

- **移設後の等価性**: 60 件の NUnit テストが `dotnet test` で全て green になることを移設完了の判定基準とする(ロジック無改変が前提)。
- **Interop DLL 化の検証**: Harness が Core.dll 経由で実 CEF を起動し、ページ読込→フレーム取得(`TryGetBuffer`)まで到達することを確認(既存 Sandbox 相当のスモーク)。
- **Unity 統合の非回帰**: Unity Editor でシーンが Core.dll を参照して起動・描画・スクロール・音声が従来どおり動くことを手動確認。プレイヤービルド(mac/win)が成立し、post-processor がネイティブ実行時を新パスからコピーできることを確認。
- **録画リプレイ**: 既存の記録(`cef_scroll_record`)を Harness で `dotnet run` リプレイし、飛び/発振が出ないことを既存 batchmode と同等に検証。

## 14. スコープ外(YAGNI)

- Interop の名前空間の抜本再設計(既存 API シグネチャは維持。名前空間統一の最小変更のみ)。
- Windows/Linux 版 `IScrollEventSource` ネイティブ源の新規実装(既存方針どおり後日)。
- 第2弾(Unity ライセンス Secrets を要する Unity 側 CI テスト)。
- ネイティブ dylib のマネージド DLL への埋め込み(不要・不採用)。

## 15. 確定済みの意思決定

- コア配置: **独立 .NET ライブラリ化 + Unity は DLL 参照**(Option B)。
- Core DLL: **LFS コミットする**。
- テスト: **dotnet 専用へ移設し Unity Test Runner 版(`Runtime.Tests.asmdef`)は廃止**。
- 既存 `cef-unity-csharp/`: **`core/` に吸収**(Sandbox → Harness)。
- ネイティブ Plugins: **案B(`Assets/CefUnity/Plugins/` へ移設、post-processor パス2箇所更新)**。
