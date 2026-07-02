# CEF-Unity Rust Project Memory

- [ゲーム開発者](user_game_developer.md) — ゲーム開発者。浅いレビューではなくメカニクス分析を求める
- [根本修正優先](feedback_proper_fix.md) — 小手先のワークアラウンドではなく根本原因から解決すること
- [敬語で対応](feedback_keigo.md) — 「です・ます」調で応対すること
- [CEF External BeginFrame 0F化](cef-external-begin-frame.md) — server-side flush で解決 (2026-06-15)。**再調査 (2026-07-02): recv フックが PostLateUpdate 末尾 = present より後で実画面は delta+1 (実質2F)。0F 化は Step1=フックを描画前へ移動 + Step2=予算適応 busy-wait (設計記録済み)**。**スクロール低下は damage-streak 検出の flush 動的抑止で解決済み (2026-07-02 実装・検証済み)。旧 rAF 138Hz バーストも同時解消 (testufo 60fps 正常化)**
- [音声遅延 実測ログ](audio-latency.md) — 内部合計 404→約160ms 達成済。次: A=producer をオーディオスレッドへ(target 80→30ms, -70〜90ms)→B=frames_per_buffer 512→C=DSP 128 で ~50〜60ms 見込み。**A の実装設計を記録済み**(OnAudioFilterRead 先頭で pull、DetachAndWait による Dispose UAF ガード、SHM read カーソルは単一スレッド限定) (2026-07-02)

## CEF Crate (v145.5.0) Key Notes

### API Version Initialization (Critical)
- CEF 145 requires `api_hash(cef::sys::CEF_API_VERSION_LAST, 0)` to be called BEFORE `initialize()` or `execute_process()`
- Without this, you get: `CefApp_0_CToCpp called with invalid version -1` (FATAL crash)
- This configures the versioned API system introduced in CEF 145

### Bundle Tool
- `bundle-cef-app` is a binary in the `cef` crate, not in user projects
- Install with: `cargo install cef --bin bundle-cef-app`
- Requires `CEF_PATH` env var pointing to the CEF framework directory (e.g., `target/debug/build/cef-dll-sys-*/out/cef_macos_aarch64`)
- Usage: `CEF_PATH=<path> bundle-cef-app <app-name>`

### Macro Patterns
- `wrap_app!`, `wrap_client!`, `wrap_life_span_handler!` etc. are defined in the bindings
- No-field syntax: `struct MyApp; impl App {}`
- With-field syntax: `struct MyApp { field: Type, } impl App {}`
- `MyStruct::new(fields...)` returns the handler type (e.g., `App`, `Client`)

### macOS LibraryLoader
- `LibraryLoader::new(exe_path, false)` for main process (looks in `../Frameworks/`)
- `LibraryLoader::new(exe_path, true)` for helper process (looks in `../../..`)
- Must keep `_loader` alive (Drop calls `unload_library`)

### Project Structure
- `[package.metadata.cef.bundle]` in Cargo.toml with `helper_name` field
- Two binary targets: main app + helper

### CEF OSR IME キャレット位置バグ (未修正)
- **問題**: `ImeCommitText` が内部で `RequestCompositionUpdates(false, false)` を呼び、composition 確定後にキャレット位置のモニタリングが停止する。以降 `on_ime_composition_range_changed` が呼ばれなくなるため、確定後のカーソル移動を追跡できない
- **Chromium 本体との差異**: 非OSR の `RenderWidgetHostViewMac` は `OnUpdateTextInputStateCalled` 内で `RequestCompositionUpdates(false, need_monitor_composition)` を呼び、テキストフィールドにフォーカスがある限り常時モニタリングを継続する。CEF OSR (`render_widget_host_view_osr.cc`) はこの処理を行っていない
- **影響**: 連続 IME 入力時（例:「夏目」確定→「漱石」入力）に候補ウィンドウが前回の位置に表示される
- **ワークアラウンド**: `on_ime_composition_range_changed` で最後の文字の右端座標を保存し、次回の初期位置として使用。初回はマウスクリック位置をフォールバックに使用
- **CEF Issue**: [#3313](https://github.com/chromiumembedded/cef/issues/3313)
- **根本修正**: CEF の `OnUpdateTextInputStateCalled` に Chromium 本体同等の `RequestCompositionUpdates` 呼び出しを追加する必要がある（CEF フォーク必要）
- **別のワークアラウンド** ([#3313 comment](https://github.com/chromiumembedded/cef/issues/3313#issuecomment-2896545087)): JS でクリック時に `caretPositionFromPoint` / 矢印キーで `selectionStart` を追跡し、V8 handler 経由でブラウザプロセスに送信。`OnFocusedNodeChanged` で編集可能ノードの bounds を取得し、両方揃った時だけ IME を有効化する方法

### Unity `Input.compositionCursorPos` 座標系
- `Input.mousePosition` と**同じ座標系** (Y=0 が画面下端)
- Y反転 (`Screen.height - y`) は**不要**（ドキュメントの記載なし、実測で確認）
- Editor の Game View Scale が影響する（Scale 2x だと座標が 1/2 にずれる）

### CEF OSR リサイズ時の再描画 (重要)
- `BrowserHost::was_resized()` だけでは再描画がトリガーされない（ページ内容に変化がないと10秒以上待つ場合がある）
- **必ず** `was_resized()` の後に `BrowserHost::invalidate(PaintElementType::VIEW)` を呼ぶこと
- `invalidate()` がビュー全体をダーティマークし、`on_paint`/`on_accelerated_paint` を強制トリガーする

### IOSurface GPU テクスチャ共有 (macOS) — 解決済み
- 詳細な設計判断・試行錯誤の記録: → [iosurface-gpu-copy.md](iosurface-gpu-copy.md)
- `on_accelerated_paint` は ARM64 macOS で動作する（`--disable-gpu-sandbox` + `shared_texture_enabled=1`）
- `IOSurfaceLookup` は macOS 11 で deprecated、macOS 16 ではプロセス間で無効化
- **Mach IPC**: `IOSurfaceCreateMachPort()` → Mach port 転送 → `IOSurfaceLookupFromMachPort()`
- **sRGB 色補正**: `newTextureViewWithPixelFormat:BGRA8Unorm_sRGB` でゼロコピー sRGB view
- **サーバーサイド GPU コピー**: Metal blit + `waitUntilCompleted` (必須)。POOL_SIZE=5
- **クロスプロセス IOSurface は `waitUntilCompleted` 以外の同期手段は不可** — パイプラインパターン、status チェック、waitUntilScheduled はすべてティアリングやロールバックを引き起こす
- **IOSurface キャッシュ**: 4エントリ（IOSurfaceID ベース）
- Unity Editor は dylib を一度ロードするとメモリに保持 → dylib 変更後は **Editor 再起動が必須**

### Obj-C @autoreleasepool (重要)
- Rust → Obj-C で Metal オブジェクトを作成する場合、**必ず `@autoreleasepool` で囲む**
- Rust スレッドには autorelease pool がない → 蓄積 → 定期バッチ解放 → フレームスパイク

### プロファイリングコードの扱い
- `on_accelerated_paint` 内のファイル I/O (fprintf, NSLog) は定期スパイクの直接原因
- 計測完了後は**必ず削除**。残す場合は 3000 フレーム以上の間隔

### ビルド注意事項
- `metal_texture.m` 変更後は `cargo clean -p cef-unity-client --release` が必要（.o キャッシュ問題）
- `deploy.sh` は release ビルドのみ（`cef-unity-rust/` ディレクトリから実行）
- Obj-C コード (metal_texture.m) の NSLog は Unity Console に表示されない。プロファイリングは Rust 側の `log_to_file` を使用
- `std::env::temp_dir()` は macOS で `/var/folders/...` を返す（`/tmp/` ではない）
