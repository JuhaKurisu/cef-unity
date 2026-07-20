# 生スクロールイベント取得 + リサンプリング (C 案) 設計書

日付: 2026-07-20
対象: cef-unity-rust/crates/client (ネイティブ層) + cef-unity-unityproject/Assets/CefUnity/Runtime (C# 層)
前提スペック: `2026-07-20-scroll-smoothing-design.md` (A 案 = ScrollSmoother。本設計はその発展)

## 背景

A 案 (指数追従 ScrollSmoother, τ=15ms) はビルド A/B で「OFF よりマシ」と確認されたが、**構造的な入力遅延** (τ 分の追従遅れ + フリック停止時に残距離が排出され続ける「浮遊感」) が残る。原因は Unity の `Input.mouseScrollDelta` がフレーム単位に量子化・合算された値しか提供せず、イベント毎のタイムスタンプ・momentum phase・ピクセル精度 delta が失われるため、Chromium が行う「フレーム表示時刻への入力リサンプリング」(層2) をクライアント側で正しく再現できないこと。

C 案は量子化前の生イベントを OS から直接取得し、Chromium `LinearResampling` 準拠のリサンプラで均一化する。追加遅延はサンプルオフセット (~5ms) のみで、momentum 終端では即座に停止する。

## スコープ

**今回実装するもの**
- `IScrollEventSource` 抽象 + 共有 `ScrollResampler` (純 C#)
- macOS ソース: NSEvent ローカルモニタ (client dylib 同梱、音声実装 au_output.c / native_voice.rs と同じパターン)
- 未対応環境・起動失敗時のフォールバック: 現行経路 (Input.mouseScrollDelta → ScrollSmoother) をそのまま維持
- 非 precise イベント (マウスホイールノッチ) は ScrollSmoother へルーティング (Chrome の層3 = ノッチアニメーション相当)

**今回実装しないもの (インターフェースに沿って後日)**
- Windows ソース: WndProc サブクラス化 (`SetWindowLongPtr(GWLP_WNDPROC)`, WM_MOUSEWHEEL/WM_MOUSEHWHEEL, 純 C# / P/Invoke のみ)。精密タッチパッド (PTP) は 120 未満の細 delta + ドライバ生成の慣性が WM_MOUSEWHEEL で届く
- Linux ソース: X11 XInput2 smooth scrolling valuator (.so, ~150 行 C)。libinput はトラックパッド慣性を生成しない (ネイティブ Linux アプリと同じ体感になる)
- Kalman / 1€ フィルタ (LinearResampling で不足した場合の発展)

## 全体構成

```
[macOS] NSEvent monitor (scroll_monitor.m ← client dylib 内)
            ↓ リングバッファ (timestamp, dx, dy, phase, precise)
        MacNativeScrollSource : IScrollEventSource   ← FFI poll (csbindgen 生成)
            ↓ 毎フレーム drain (OnEarlyUpdateLast)
        precise イベント → ScrollResampler (新規・純C#) ─┐
        非precise (ホイールノッチ) → ScrollSmoother (既存A) ─┤→ SendMouseWheel
[未対応環境/起動失敗] FramePolled fallback (現行経路) ────┘
```

3 経路とも最終的に同じ `SendMouseWheel` に合流する。native モード時は `Input.mouseScrollDelta` を無視する (二重計上防止)。

## ネイティブ層 (cef-unity-rust/crates/client)

### scroll_monitor.m (~120 行)

- `[NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskScrollWheel handler:...]` でスクロールイベントを購読。ハンドラはイベントを **素通し (return event)** し、Unity 側の通常配送を妨げない
- ハンドラは AppKit メインスレッドで発火し、Unity スクリプト (ポーリング側) も同じメインスレッド → **ロック不要の単純配列リング** (容量 256、飽和時は古いものを捨てる)
- 記録フィールド: `timestamp` (NSEvent.timestamp = 起動からの秒)、`scrollingDeltaX/Y` (precise ならピクセル精度)、`phase` / `momentumPhase` (began/changed/ended/cancelled のビット表現)、`hasPreciseScrollingDeltas`
- 権限不要 (自アプリ宛イベントのみ)。モニタ登録失敗時は start が 0 を返す

### scroll_monitor.rs (FFI 4 本、#[no_mangle])

```
cef_scroll_monitor_start() -> i32          // 1=成功 0=失敗
cef_scroll_monitor_stop()
cef_scroll_monitor_poll(out: *mut CefScrollEvent, max: i32) -> i32   // 新着イベント数
cef_scroll_monitor_now() -> f64            // イベントと同一クロックの現在時刻 (秒)
```

`CefScrollEvent` 構造体 (repr(C)) は csbindgen が C# 側 struct を自動生成する。`build.rs` に `cc::Build` (macOS のみ cfg) を 1 エントリ追加。**dylib 変更のため Editor 再起動が必要 (既知の制約)。deploy.sh でビルド・配置**。

## C# 層

### 抽象 (Assets/CefUnity/Runtime/ScrollInput/)

```csharp
public struct ScrollInputEvent { public double Timestamp; public float DxPx, DyPx; public bool Precise; public ScrollPhase Phase; }
public enum ScrollPhase { None, GestureBegan, GestureChanged, GestureEnded, MomentumBegan, MomentumChanged, MomentumEnded, Cancelled }
public interface IScrollEventSource : System.IDisposable {
    bool Start();                          // false → フォールバック
    int Poll(ScrollInputEvent[] buffer);   // 新着イベントを buffer に書き、件数を返す
    double Now { get; }                    // イベントと同一クロックの現在時刻 (秒)
}
```

- `MacNativeScrollSource : IScrollEventSource` — DllImport 呼び出しの薄いラッパ。ネイティブ struct → `ScrollInputEvent` 変換 (phase ビット → enum)
- Windows/Linux ソースは後日このインターフェースを実装する

### ScrollResampler (純 C#、EditMode テスト可能)

Chromium `LinearResampling` (出荷既定) 準拠:

- イベントから軸ごとの累積位置 P(t) を構築 (直近 2 イベントの (t, P) を保持)
- 毎フレーム `sampleTime = source.Now − SampleOffset (5ms)` の P を求める:
  - 2 イベント間なら**線形補間**
  - 最終イベントより後なら**線形外挿** (上限 ExtrapolationCap = 8ms。超過分は最終イベント位置で保持 — オーバーシュート防止)
- `delta = P(sampleTime) − P(前回 sampleTime)` を int px で排出。端数は繰り越し (総量保存)
- **momentumPhase = Ended / Cancelled、または GestureEnded 後に momentum が始まらないまま次イベント無し**: 残差 (P(最終イベント) − P(前回サンプル)) を即座に全排出して履歴クリア → 「止めたのに動き続ける」が構造的に消える
- **グレース終端**: phase 終了イベントを取り逃しても、100ms 無イベントで終端扱い (残差排出 + クリア)
- 追加遅延は SampleOffset の ~5ms のみ (A 案の τ=15ms + テールより大幅に低遅延)

### delta スケール

precise の `scrollingDeltaX/Y` は CSS px 相当 → ブラウザ view 座標へは `× _resolutionScale` のみ。`WheelPixelsPerStep` (60) は**非 precise (ライン単位ノッチ) 専用**で、非 precise イベントの delta に掛けて ScrollSmoother へ渡す。

## 統合 (CefUnityBrowserSample)

- 初期化時 (browser 生成後) に `MacNativeScrollSource.Start()` を試行:
  - 成功 → native モード。`HandleMouseInput` の `Input.mouseScrollDelta` 蓄積をスキップ
  - 失敗 → 現行経路のまま (ログ 1 回)
- 毎フレーム OnEarlyUpdateLast (現 `TickScrollSmoother` の位置で、その直前):
  1. `Poll` で全イベント drain (リング鮮度維持のため常に drain)
  2. **カーソルがブラウザ上 (既存 `TryGetBrowserCoord` 成功) のときだけ** イベントを転送: precise → Resampler / 非 precise → ScrollSmoother (×WheelPixelsPerStep)。これにより **Editor で他ウィンドウ上のスクロールを拾う quirk も自然に解消**
  3. Resampler をサンプルし、非 0 なら `SendMouseWheel` (最終有効座標、`_inputSentThisFrame = true` — 既存 BeginFrame/0F-wait 経路に乗る)
  4. 既存 `TickScrollSmoother` は従来通り動く (非 precise / フォールバックの排出)
- `OnDestroy` で `Dispose` (monitor stop)。`LoadUrl` で Resampler と Smoother 両方をリセット
- Editor でも native モードで動作する (play mode 中のみモニタ起動)

### 開発トグル (既存の #if UNITY_EDITOR || DEVELOPMENT_BUILD 群に追加)

- `cef_scroll_legacy`: native ソースが使えても強制的にフォールバック経路にする — A/C の体感 A/B 用

## エッジケース

| ケース | 扱い |
|---|---|
| モニタ起動失敗 / ヘッドレス | Start()=false → フォールバック (ログ 1 回) |
| ジェスチャ中のナビゲーション | `LoadUrl` で Resampler+Smoother をリセット |
| アプリ非アクティブ | ローカルモニタは自アプリ宛イベントのみ受信 → 自然に停止 |
| イベント取り逃し (phase 終了が来ない) | 100ms 無イベントのグレース終端 |
| リング飽和 (256 超) | 古いイベントから破棄 (poll は毎フレームなので実質発生しない) |
| 方向反転 | P(t) の単調性に依存しない (補間は座標そのまま) |
| dylib 更新 | Editor 再起動必須 (既知)。deploy.sh でビルド |

## テストと検証

1. **ScrollResampler EditMode テスト** (合成イベント列、Runtime.Tests):
   - 120Hz 均一 momentum 減衰列 → per-frame 排出が滑らか (隣接差が小さい)
   - イベントタイミングジッター (±3ms) → 排出は均一のまま (リサンプルの本質)
   - 補間/外挿の境界、外挿上限 8ms で停止
   - momentumEnded で残差即排出 + 以後 0
   - グレース終端 (100ms 無イベント)
   - 方向反転、端数繰り越しの総量保存
   - ※テスト実行は batchmode CLI (`-runTests -testPlatform EditMode`)。uloop run-tests はセキュリティブロック
2. **dev ビルド実測**: probe CSV (`cef_perf_probe`) で per-frame 均一性を確認し、`cef_scroll_legacy` トグルで A/C を切り替えてユーザー体感 A/B。**成功基準: 遅延感の解消 (τ 遅れ・停止後の浮遊感が消える) + 滑らかさが A 案以上**
3. **リリース経路コンパイル検証**: BuildOptions.None でビルド成功 (native モード自体はリリースでも有効。dev トグルのみ #if)
4. 検証は実ユーザーの手動スクロールで行う (programmatic 注入では CEF が paint しない計測の罠)

## 決定事項の記録

- ネイティブ配置: 既存 Rust client dylib 同梱 (1-A)。独立 .bundle は新ビルド経路が増えるため不採用
- リサンプラ: Chromium LinearResampling 準拠 (2-A)。LSQ/Kalman は不足時の発展
- スコープ: 抽象 + macOS のみ (ユーザー決定 2026-07-20)。未対応環境は A 案フォールバック
