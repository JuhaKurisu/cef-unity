# CefUnity.Viewer

Unity 外で CEF を表示+操作する mac 単体ブラウザ。スクロールカクツキの
Unity 固有性切り分けが主目的 (spec: docs/superpowers/specs/2026-07-25-silknet-viewer-design.md)。

## 実行

```
dotnet run --project core/CefUnity.Viewer -- [--url <url>] [--size 1280x720]
    [--scroll-mode raw|smoother|resampler] [--record]
    [--replay <events-csv>] [--statistics <output-csv>] [--analyze <statistics-csv>]
dotnet run --project core/CefUnity.Viewer -- spike   # SDL/Metal/NSEvent/IME 疎通確認
```

## 実行時ショートカット

| キー | 動作 |
|---|---|
| F1 / F2 / F3 | スクロールモード切替 raw / smoother / resampler (タイトルに表示) |
| F5 | 生イベント録画トグル → $TMPDIR/cef_scroll_events.csv |

## 切り分けの実験プロトコル

1. Unity (または Viewer) で `--record` 相当の録画を取る
2. `--replay <csv> --statistics <out.csv>` で同一入力を Viewer に再生 (再生中は窓をアクティブに保つ — 非アクティブだと CEF paint が凍結)
3. `--analyze <out.csv>` で粗さ指標を計算し、ホスト間で比較する
   (比較は必ず本コマンド同士で行う — 過去のアドホック集計値との直接比較はしない)

## トラブルシューティング

- サーバープロセス残留 (次回起動が永久ハング): `pkill -f cef-unity-server`
- 起動ハング (キャッシュ破損): `$TMPDIR` 配下の cef_unity_cache を削除
- スクロール resampler モードが効かない: 起動ログの `native scroll source:` を確認
- モメンタムスクロールが効かない: 初回起動時に `~/Library/Preferences/CefUnity.Viewer.plist` へ `AppleMomentumScrollSupported=YES` を永続書き込みする (SDL がモメンタムスクロール配送を止めるための対策。除去は `defaults delete CefUnity.Viewer AppleMomentumScrollSupported`)
