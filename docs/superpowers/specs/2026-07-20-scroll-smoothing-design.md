# スクロール入力平滑 (クライアント側指数追従) 設計書

日付: 2026-07-20
対象: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs` のホイール入力経路

## 背景 (診断の要約)

体感スクロールカクツキの真因は描画側ではなく入力側にある (2026-07-20 スタンドアロンビルドでのクリーン計測で確定):

- 重いページ (画像500枚・box-shadow 4000) でも Unity present は mean 16.77ms / std 1.04ms、CEF paint 57fps とほぼ完璧な 60fps
- 一方、手動トラックパッドスクロールの 1 フレームあたり送信量 (sentDy) は -1138px 〜 +306px と乱高下。フリック除外の穏やかな区間でも CoV=0.82 (0=完全均一)、28% のフレームが >100px の単発ジャンプ
- 現行実装は `Input.mouseScrollDelta` を毎フレーム生のまま `SendMouseWheel` に 1:1 転送しており、per-frame 移動量の不均一がそのまま画面に出る
- Chrome/Arc は入力を vsync に整列してリサンプル・予測する層 (enable-resampling-scroll-events, Kalman/LSQ) を持つため滑らか。CEF OSR には Unity フレームで量子化済みの合成イベントしか渡らないため、この層はクライアント (Unity) 側で実装するしかない

## ゴール / 非ゴール

**ゴール**
- per-frame のスクロール送信量を均一化し、Chrome 相当の滑らかな体感にする
- フリックの巨大単発 (数百〜千 px/frame) を複数フレームの減衰グライドに分散する
- 総スクロール量は実質無損失 (排出合計 = 入力合計。終端の 0.5px 未満の端数破棄のみ許容)
- 平滑強度 τ をビルドの再作成なしに体感チューニングできる (暫定機構)

**非ゴール (今回)**
- 生入力イベント取得によるリサンプル (C 案)。将来ほぼ確実に実装するが今回は対象外。mac (NSEvent monitor) / Windows (WndProc subclass) / Linux (XInput2) の手法はプロジェクトメモリ `scroll-stutter-diagnosis.md` に記録済み
- Blink 側スムーズスクロール (`--enable-smooth-scrolling`) の検証
- タッチジェスチャ / キーボードスクロール

## 設計

### 1. 構成と分離

平滑ロジックを純 C# クラス `ScrollSmoother` (新規 `Assets/CefUnity/Runtime/ScrollSmoother.cs`、Unity API 非依存) に切り出す。

```
ScrollSmoother
  AddInput(float dxPx, float dyPx)                    // 入力を残距離に加算
  Tick(float dt, float tau, out int dx, out int dy)   // 1 フレーム分の排出量を計算
  Reset()                                             // 残距離破棄
  bool IsActive                                       // 残距離あり (排出継続判定)
```

分離の理由:
- EditMode ユニットテストが書ける (Play mode + CEF 不要)
- 将来 C 案に移行する際、「毎フレーム排出」の継ぎ目を維持したまま中身を ScrollResampler + IScrollEventSource に差し替えられる

### 2. アルゴリズム

- 残距離 `_remainX/_remainY` (float px) を保持。`AddInput` で加算 (方向反転は符号の相殺で自然に処理)
- `Tick(dt, tau)`:
  - `tau <= 0` → 全量即排出 (平滑 OFF = 現行挙動と等価。A/B 比較用)
  - `k = 1 − exp(−dt/tau)` で `step = remain × k` を計算
  - step を int に丸めて排出し、排出した int 分だけ remain から減算 (端数は remain に残る → 総移動量無損失。現行 `_wheelAccum` の端数保持を統合)
  - 終端: `|remain| <= 1px` なら `Round(remain)` を排出して remain = 0 (無限テール防止。丸め結果が 0 の場合は排出なしで破棄 — 最大 0.5px 未満の損失は許容)
- 参考特性 (τ=45ms, 60fps): 初フレームで残りの約31%が動く (食いつき即座)。フリック 1138px は約15フレーム (~250ms) の幾何減衰グライドに分散

### 3. 統合点とデータフロー

```
HandleMouseInput:   Input.mouseScrollDelta × WheelPixelsPerStep × _resolutionScale
                     → smoother.AddInput(...)     (現行の即時 SendMouseWheel を廃止)
OnEarlyUpdateLast:  HandleMouseInput の後・BeginFrame#1 の前の独立ステップとして
                     smoother.Tick(dt, τ) → 非0なら SendMouseWheel(_lastMouseX, _lastMouseY, dx, dy, mods)
                     → _inputSentThisFrame = true  (既存の BeginFrame / 0F-wait 経路に乗る)
```

排出を `HandleMouseInput` の外に置く理由: 現行コードは `TryGetBrowserCoord` 失敗 (カーソルがブラウザ外) で早期 return するため、中に置くとグライド途中でカーソルが外れた瞬間に排出が止まる。独立ステップにして最後の有効座標 `_lastMouseX/_lastMouseY` で送り続ける。

### 4. エッジケース

| ケース | 扱い |
|---|---|
| ナビゲーション / ブラウザ再生成 | `LoadUrl` と browser 再生成時に `Reset()` (残距離を新ページへ流し込まない) |
| 排出中の modifier 変化 | 排出時点の `GetCefModifiers()` を使用 (Chrome と同挙動) |
| `cef_scroll_slow` / `cef_scroll_test` 注入 | 従来通り直接 `SendMouseWheel` (平滑バイパス。均一性テスト用途) |
| probe 計装 `_frameSentDy` | 排出値を記録する位置へ移動 (平滑後の実送信量を観測) |
| 離散マウスホイール | 同一経路で自然に平滑 (Chrome の 140ms ノッチアニメ相当の効果) |
| 横スクロール | X 軸も同一機構で処理 |

### 5. τ チューニング機構 (暫定)

- 既定 `τ = 0.045f` 秒 (const、= 45ms)
- temp ファイル `$TMPDIR/cef_scroll_tau` (**ms 単位**のテキスト。例: `45`) があれば上書き (内部で秒に変換)。60 フレームに 1 回だけチェック (毎フレーム I/O 回避)。`0` で平滑 OFF
- 体感確認で最終値が決まったら const に固定し、ファイル機構は既存の一時計装群と共に撤去する

### 6. テストと検証

1. **EditMode ユニットテスト** (`ScrollSmootherTests.cs`、uloop-run-tests で実行):
   - 総量実質無損失 (排出合計 = 入力合計 ± 0.5px 未満)
   - 幾何減衰 (排出量が単調減少)
   - 方向反転 (逆符号入力で残距離が相殺)
   - 終端スナップ (残 1px 以下で全量排出・以後 0)
   - τ=0 で即時全量排出 (現行挙動)
   - dt 依存性 (同じ実時間なら分割数に依らず同等の排出)
2. **ビルド実測**: 手動スクロールを probe CSV (`cef_perf_probe` → `$TMPDIR/cef_perf.csv`) で記録し、per-frame sentDy の CoV 0.82 → 0.3 未満への低下を確認
   - 注意: programmatic 注入では CEF が paint しない計測の罠があるため、検証は必ず実ユーザーの手動スクロールで行う
3. **体感確認**: `cef_scroll_tau` を 0 (OFF) / 25 / 45 / 80 で切り替えて Arc と比較

## 将来拡張 (C 案への道筋)

`ScrollSmoother` の「毎フレーム排出」インターフェースを保ったまま、入力側を `IScrollEventSource` (per-event timestamp + delta + phase) に、平滑を LSQ 速度推定 + フレーム表示時刻への再標本化に差し替える。プラットフォーム別イベントソース: macOS = NSEvent local monitor (.bundle)、Windows = WndProc subclass (純 C#)、Linux = XInput2 (.so)。詳細はプロジェクトメモリ参照。
