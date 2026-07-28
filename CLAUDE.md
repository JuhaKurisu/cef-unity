# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## 命名規約 (Rust / C# 共通)

識別子 (変数・関数・引数・フィールド・定数・型) は省略形を使わず、常にフルネームで書くこと。

- 例: `bf`→`begin_frame`, `buf`→`buffer`, `idx`→`index`, `msg`→`message`, `recv`→`receive`, `accel`→`accelerated`, `shm`→`shared_memory`, `init`→`initialize`, `stats`→`statistics`, `_ms`/`Ms` 接尾辞→`milliseconds`/`Milliseconds`, `dx`/`dy`→`delta_x`/`delta_y` (C#: `deltaX`/`deltaY`), `w`/`h`→`width`/`height`。ループ変数も `i` ではなく `index`, `frame_index` 等
- 維持してよいもの: 辞書化された語 (app, config, info, max, min, delta, sync, log)、普遍的な頭字語 (id, url, ime, gpu, fps, ipc, ffi, osr, cef, d3d11, bgra, io, dsp, rms)、座標の `x`/`y`、`tau` (時定数の正式名)、`flink` (shared_memory クレートの用語)
- 改名不可: 外部 API 名、CEF トレイト実装メソッド、Unity マジックメソッド、`UnityPluginLoad`/`UnityPluginUnload`、csbindgen 生成物 (`NativeMethods.g.cs`)、`-executeMethod` で外部参照される完全修飾名 (`CefUnity.Editor.CefQuickBuild.BuildMac`, `CefUnity.Editor.ScrollReplay.Run`)
- 文字列リテラルは挙動契約なので変更しない: dev トグル ("cef_scroll_legacy" 等)、プロトコル文字列 ("\_\_CARET\_\_")、CSV フォーマット、CLI 引数、環境変数名

## サブディレクトリの指針

- Rust 側のビルド・デプロイ手順: `cef-unity-rust/CLAUDE.md` を参照 (Rust 変更時は csbindgen 再生成 → `deploy.sh` / `deploy.ps1` 必須)
- C# Interop の単一の真実源は `cef-unity-csharp/CefUnity.Core/Interop/` (`NativeMethods.g.cs` は `crates/client/build.rs` の csbindgen がここへ直接生成する)。Unity は `Assets/CefUnity/Plugins/CefUnity.Core.dll` を参照するため、C# 側変更時は `bash cef-unity-csharp/build-csharp.sh` で再ビルド・配置する
