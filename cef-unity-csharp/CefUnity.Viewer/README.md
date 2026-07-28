# CefUnity.Viewer

Unity 外で CEF を表示+操作する macOS / Windows 単体ブラウザ。スクロールカクツキの
Unity 固有性切り分けが主目的 (spec: docs/superpowers/specs/2026-07-25-silknet-viewer-design.md、
Windows 対応: docs/superpowers/specs/2026-07-28-windows-viewer-d3d11-design.md)。

表示はどちらも GPU ゼロコピー経路を使う (macOS: IOSurface → Metal blit / Windows: D3D11 共有
テクスチャ → DXGI スワップチェーン)。

## 実行

```
dotnet run --project cef-unity-csharp/CefUnity.Viewer -- [--url <url>] [--size 1280x720]
    [--scroll-mode raw|smoother|resampler] [--record]
    [--replay <events-csv>] [--statistics <output-csv>] [--analyze <statistics-csv>]
dotnet run --project cef-unity-csharp/CefUnity.Viewer -- spike   # SDL/Metal/NSEvent/IME 疎通確認
```

## 実行時ショートカット

| キー | 動作 |
|---|---|
| F1 / F2 / F3 | スクロールモード切替 raw / smoother / resampler (タイトルに表示) |
| F5 | 生イベント録画トグル → $TMPDIR/cef_scroll_events.csv |

Windows にはネイティブスクロールソース (macOS の NSEvent モニタ相当) が無く、Resampler モードは
窓の wheel イベントを無視する仕様のため、起動時に自動的に Smoother へ落ちる (F1/F2/F3 で切替可)。

## 切り分けの実験プロトコル

1. Unity (または Viewer) で `--record` 相当の録画を取る
2. `--replay <csv> --statistics <out.csv>` で同一入力を Viewer に再生 (再生中は窓をアクティブに保つ — 非アクティブだと CEF paint が凍結)
3. `--analyze <out.csv>` で粗さ指標を計算し、ホスト間で比較する
   (比較は必ず本コマンド同士で行う — 過去のアドホック集計値との直接比較はしない)

## トラブルシューティング

- サーバープロセス残留 (次回起動が永久ハング): macOS は `pkill -f cef-unity-server`、
  Windows は `taskkill /IM cef-unity-server.exe /F`
- 起動ハング (キャッシュ破損): macOS は `$TMPDIR`、Windows は `%TEMP%` 配下の cef_unity_cache を削除
- スクロール resampler モードが効かない: 起動ログの `native scroll source:` を確認
- 入力が一切効かない: 起動ログの `input devices: mice=N keyboards=N` が 0 件でないか確認
- Windows で黒画面のまま: `%TEMP%\cef_unity_debug.log` の `external d3d11 device set` /
  `opened handle=` 行を確認する (デバイス注入と共有テクスチャの open が成功しているか)

## 既知の制限

- **Windows の高 DPI ディスプレイ**: Viewer は DPI 非対応プロセスとして動くため、表示スケールが
  100% でない環境では OS がウィンドウを引き伸ばして表示する (150% なら 1000x700 の窓が物理
  1500x1050 になり、その分ぼやける)。入力座標と CEF 側の座標は一致するので操作に支障はない。
  等倍で見たい場合は実行ファイルのプロパティ → 互換性 → 高 DPI 設定で「アプリケーション」を選ぶ
- モメンタムスクロールが効かない: 起動時に毎回 (冪等) `~/Library/Preferences/CefUnity.Viewer.plist` へ `AppleMomentumScrollSupported=YES` を書き込む (SDL がモメンタムスクロール配送を止めるための対策。除去は `defaults delete CefUnity.Viewer AppleMomentumScrollSupported`)

## 録画・リプレイの注意事項

- `--record` は既存の録画 CSV (`$TMPDIR/cef_scroll_events.csv`) を削除して新規開始する (旧セッションの追記混入防止)
- `--replay` 可能な録画 (over=1) が残るのは Resampler モード時のみ (Raw/Smoother 中のイベントは over=0 で記録され再生対象外)
- リプレイ中のモード切替 (F1/F2) は再生イベントが破棄されるため非推奨
