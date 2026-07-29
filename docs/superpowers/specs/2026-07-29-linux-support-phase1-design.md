# Linux サポート フェーズ 1 — Rust 単体の Linux 動作 設計書

作成日: 2026-07-29

## 到達ライン

Linux (x86_64) で `cef-unity-server` と `cef-unity-rust-helper` が起動し、software paint 経路で
共有メモリに BGRA フレームが載り、**C# Harness (`CefUnity.Harness`) から読み出せる**ところまで。

Unity Editor / Player 対応はフェーズ 2 以降に分離する。

## 前提調査の結果

WSL2 (Ubuntu 24.04) 上で実測した結果、Linux 対応に必要な作業は当初想定よりはるかに小さい。

| 確認項目 | 結果 |
|---|---|
| `cargo build` (workspace) | **1 箇所の修正で成功**。client / ipc / helper は無修正で通る |
| `cargo test -p cef-unity-ipc` | **19/19 パス** (共有メモリ read/write ラウンドトリップを含む) |
| CEF Linux 配布物 | `cef-dll-sys` が自動取得・展開する (`target/*/build/cef-dll-sys-*/out/cef_linux_<arch>/`。<br>x86_64 環境での実測値は `cef_linux_x86_64`)。<br>`libcef.so`, `*.pak`, `icudtl.dat`, `locales/`, SwiftShader 一式のフラット構成 |
| `libcef.so` のリンク | 実行時に **解決できない** (RPATH/RUNPATH なし) |
| `libcef.so` の実行時依存 | `libnss3` / `libnspr4` / `libasound2` が不足 (apt で解消) |
| CEF ロード | 上記解消後、サーバーは正常起動し `--ipc-server argument required` で停止 = **CEF ロードは通る** |

既存コードが既に Linux を織り込み済みの箇所:

- `server.rs:334` — `shared_texture_enabled` を立てるのは macOS / Windows のみ。
  Linux は自動的に software paint (`on_paint` → 共有メモリ BGRA) に落ちる
- `server.rs:957` — `settings.no_sandbox = 1` が無条件。**`chrome-sandbox` の SUID 設定は不要**
- `server.rs:983` — 非 macOS の `resources_dir_path` は実行ファイルと同一ディレクトリ。
  CEF Linux 配布物のフラット構成とそのまま一致する
- `event_loop/generic.rs` — condvar ベースのポーリングループ。冒頭コメントどおり Linux で使える
- `d3d11_pool` — 非 Windows ではスタブが常に `Err` を返し `None` → software 経路へ
- `server.rs:1758` / `client/src/lib.rs:109` — `helper_binary_path` / `server_binary_path` の
  Linux 分岐は記述済み (未検証)

## アーキテクチャ判断

**Linux 固有のサブシステムを新設しない。** 既存の「非 macOS = software paint + generic event loop」
経路を Linux に開通させるだけとする。

GPU ゼロコピー (dmabuf / EGL) は macOS の IOSurface、Windows の D3D11 共有テクスチャに相当する
第 3 の経路になるが、フェーズ 1 では扱わない。software paint で正しく動く土台を先に確定させる。

## CPU アーキテクチャの扱い

既存コードにアーキテクチャ分岐 (`#[cfg(target_arch)]`) は存在しない。クライアントは
プラグインディレクトリを `dylib_directory()` (`client/src/lib.rs:272`) — 自分自身が
ロードされた場所 — として解決するため、ネイティブ側はアーキテクチャ非依存に作られている。
アーキテクチャ名が現れるのは deploy スクリプトの配置先 (`osx-arm64` / `win-x64`) だけで、
これは Unity のプラグインフォルダ規約に従ったものである。

フェーズ 1 でもこの方針を踏襲し、アーキテクチャ分岐を導入しない。CEF 配布物の探索には
既存の `cef_macos_*` と同じくワイルドカードを使い、x86_64 / aarch64 のどちらでも
同じスクリプトが通るようにする。

**サポート対象アーキテクチャの方針 (2026-07-29 決定):** Intel Mac (x86_64 macOS) は
サポートしない。`deploy.sh` の配置先が `osx-arm64` にハードコードされているのは
この方針に沿った意図的なものであり、修正対象ではない。この方針は
`cef-unity-rust/CLAUDE.md` にも記載する (変更点 5 に含める)。

## 変更点

### 1. `crates/server/src/server.rs:1174-1180` — `cef_window_handle_t` の Linux 分岐

現状のコメント「macOS / Linux: `*mut c_void`」は Linux について誤り。`cef-dll-sys` の
ターゲット別バインディングでは:

| ターゲット | 型 |
|---|---|
| macOS | `*mut c_void` |
| Linux | `c_ulong` (X11 Window / XID) |
| Windows | `HWND` |

`#[cfg(not(target_os = "windows"))]` で macOS と Linux をまとめている箇所を 3 分岐に割る。

```rust
#[cfg(target_os = "windows")]
let parent_handle = cef::sys::HWND(std::ptr::null_mut());
#[cfg(target_os = "macos")]
let parent_handle = std::ptr::null_mut();
#[cfg(target_os = "linux")]
let parent_handle: cef::sys::cef_window_handle_t = 0;
```

WSL2 上で適用してビルド成功を確認済み。

### 2. RPATH の付与 — `crates/server/build.rs` と `crates/helper/build.rs` (新規)

`libcef.so` を実行ファイルと同じディレクトリから解決させる。

```rust
#[cfg(target_os = "linux")]
println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
```

これにより配布ディレクトリ内で `LD_LIBRARY_PATH` なしに起動できる。macOS は framework の
動的ロード、Windows は同一ディレクトリ探索で解決済みなので、Linux だけの追加となる。

`crates/helper` には現在 `build.rs` が存在しないため新規作成する。

### 3. `build-server-sandbox.sh` — OS 分岐の追加

現状は macOS 専用 (`.app` バンドル生成 + `codesign`)。OS 判定を先頭に置き、Linux では
フラット配置を行う分岐を足す。バンドル生成ロジックは既存のまま `darwin` 側に閉じる。

Linux 側が出力先ディレクトリへ配置するもの:

- `target/debug/cef-unity-server`
- `target/debug/cef-unity-rust-helper`
- CEF 配布物ディレクトリから `libcef.so`, `libEGL.so`, `libGLESv2.so`, `libvk_swiftshader.so`,
  `libvulkan.so.1`, `vk_swiftshader_icd.json`
- 同ディレクトリから `*.pak`, `icudtl.dat`, `v8_context_snapshot.bin`, `locales/`

CEF 配布物ディレクトリの特定には、既存の macOS 分岐 (`build-server-sandbox.sh:16` の
`cef_macos_*`) と同じくワイルドカード `cef_linux_*` を使う。x86_64 と aarch64 の
どちらでも同じスクリプトが通り、アーキテクチャ名をハードコードしない。

`chrome-sandbox` は `no_sandbox = 1` のため配置しない。

### 4. `cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj` — 共有ライブラリ名の OS 分岐

2 箇所ある。

**(a) 共有ライブラリ名 (14 行目)** — `libcef_unity_rust.dylib` 決め打ち。MSBuild の OS 条件で
`.so` / `.dylib` を切り替える。

**(b) パス区切り文字 (7-8 行目)** — `RustProjectDir` / `RustTargetDir` が
`'$(MSBuildThisFileDirectory)..\..\cef-unity-rust\target\debug'` とバックスラッシュ区切りで
書かれている。一貫性のためスラッシュ区切りに直す。

> **訂正 (2026-07-29、実装後の実測による):** 当初この設計書は「Linux の MSBuild は
> バックスラッシュを区切りとして扱わないため `Path.GetFullPath` が誤ったパスを返す」と
> 記述していたが、**これは誤りだった**。Unix の MSBuild は `Path.GetFullPath` に渡された
> 文字列内のバックスラッシュを正規化するため、両表記は同一のパスに解決される
> (Linux 上で `dotnet msbuild -t:Show` により実測)。したがってこの変更は
> 全プラットフォームで挙動を変えない純粋な一貫性の改善であり、不具合修正ではない。

`CopyServerApp` ターゲット (18-20 行) は `build-server-sandbox.sh` 呼び出しのままでよい
(スクリプト側で OS 分岐するため)。

### 5. `cef-unity-rust/CLAUDE.md` — Linux セクションの追記

Linux で開発する際の前提を記録する:

- 必要な apt パッケージ: `build-essential pkg-config curl git cmake python3` (ビルド)、
  `libnss3 libnspr4 libasound2t64 libatk1.0-0t64 libatk-bridge2.0-0t64 libcups2t64 libdrm2
  libgbm1 libgtk-3-0t64 libxcomposite1 libxdamage1 libxfixes3 libxkbcommon0 libxrandr2
  libpango-1.0-0 libcairo2 libx11-xcb1 libxss1` (CEF 実行時)
- フェーズ 1 では Unity への deploy スクリプトは用意しない (Harness のみ)
- Linux は software paint 経路のみ。GPU ゼロコピーは未実装

## 検証手順

1. `cargo build` — ワークスペース全体がビルドできる
2. `cargo test -p cef-unity-ipc` — 19 件パス
3. `readelf -d target/debug/cef-unity-server | grep RUNPATH` — `$ORIGIN` が入っている
4. `build-server-sandbox.sh <dir>` — 配置物が揃い、`ldd` に `not found` がない
5. Harness を実行し、CEF 初期化 → ブラウザ生成 → ページロード → `on_paint` 経由で
   共有メモリにフレームが載ることを確認。取得した BGRA を PNG に書き出して目視確認する

5 番が本フェーズの合否判定となる。

## 未知のリスク

以下はコード読解では判定できず、実際に動かして初めて分かる。フェーズ 1 の作業の大半は
ここの解消に費やされる可能性がある。

- **ozone / X11 バックエンド** — OSR とはいえ Chromium は表示バックエンドを要求する。
  WSLg が `DISPLAY=:0` を提供するため X11 で通る見込みだが未検証。失敗する場合は
  `--ozone-platform=headless` または `--disable-gpu` を command line switch に追加する
  (`server.rs:777` に `disable-gpu-sandbox` を足している箇所があり、同じ場所で対処できる)
- **zygote プロセス** — Linux の CEF は zygote プロセスを fork する。helper の
  `execute_process` が `--type=zygote` を正しく捌けるか未検証
- **external BeginFrame + software paint** — `external_begin_frame_enabled = 1` の状態で
  Linux の software paint が期待どおり駆動されるか未検証

いずれも「発生したら command line switch の追加で対処する」性質のもので、アーキテクチャの
変更を要する類ではないと見込んでいる。

## スコープ外

- Unity Editor / Player の Linux 対応、`Assets/CefUnity/Plugins/linux-x64/` への deploy
- GPU ゼロコピー経路 (dmabuf / EGL)
- ネイティブ音声出力 (macOS の AudioUnit 経路に相当するもの)。Linux は Unity ミキサ経路のみ
- GitHub Actions の ubuntu ジョブ追加

## 実装結果 (2026-07-29)

**フェーズ 1 は完了した。** 合否判定 (検証手順 5) は PASS —
`SMOKE_OK frames=3` を得て、`dump` が出力した PNG に example.com が正しく描画されていることを
目視確認した (リンクが青く出ており BGRA→RGB の並べ替えも正しい)。

**「未知のリスク」は 3 つとも顕在化しなかった。** ozone/X11、zygote、external BeginFrame の
いずれについても command line switch の追加は不要で、`server.rs` は Task 1 の型分岐以外
一切変更していない。既存コードの `#[cfg(not(target_os = "macos"))]` 経路がそのまま
Linux で機能した。

実装中に判明した、設計時に見えていなかったもの:

- `CefUnity.Harness/Directory.Build.targets` と `CefUnity.Viewer/Directory.Build.targets` の
  `CopyCefFramework` が `OSX Or Linux` 条件で macOS 専用の `cef_macos_aarch64` を参照しており、
  Linux で `rsync` が失敗していた。両方とも `OSX` 限定に修正した (設計書の変更点には無かった)

## フェーズ 2 への申し送り

- **フレーム供給レート**: 600 反復 (約 10 秒) で `frames=3` と少ない。`get_active_buffer_pointer`
  が `frame_id` の変化で edge-trigger され、CEF は damage が無ければ再描画しないため静的ページ
  では妥当な形だが、スクロール等の動的な負荷での供給レートは未確認
- **真のヘッドレス環境**: 検証は WSLg (X11 が見える状態) で行った。X11 が無い環境では
  `--ozone-platform=headless` が必要になる可能性がある (`cef-unity-rust/CLAUDE.md` に記載済み)
- **PNG ライターのテストの穴**: CRC 計算自体のテストが無く (IEND の既知 CRC `0xAE426082` との
  照合で塞げる)、多画素ケースが IDAT を展開検証していないため行ストライド計算が間接検証のみ
- **`CefUnity.Harness/Directory.Build.targets`** には Viewer 側にある
  `Exists('$(_CefRustTargetDir)/build')` ガードが無い (既存の非対称、本フェーズ以前からのもの)
- **`build-server-sandbox.sh` の CEF 配布物探索**が `ls -d ... | head -1` で辞書順選択になっている
  (`ls -dt` なら新しい順)。macOS 分岐も同じで、直すなら両方
- **GPU ゼロコピー (dmabuf / EGL)**: macOS の IOSurface、Windows の D3D11 共有テクスチャに
  相当する第 3 の経路。フェーズ 1 では扱わなかった
- **Unity 対応**: `Assets/CefUnity/Plugins/linux-x64/` への deploy スクリプトが未整備

## 開発環境

WSL2 (Ubuntu 24.04 LTS) を使う。ディストロは `F:\WSL\Ubuntu-24.04` に配置済み。

作業ツリーは git worktree で隔離する: `F:\GitHub\cef-unity\.claude\worktrees\linux-phase1`
(WSL からは `/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase1`)。並行して進む
Windows arm64 対応 (`feat/windows-arm64-wt`) と作業ツリーを共有しないための措置。

`target/` だけは ext4 側に逃がす (`CARGO_TARGET_DIR=$HOME/cef-target-mnt`)。ビルド生成物の
I/O が支配的なので、これだけで `/mnt/f` 越しのビルドが実用速度になる (実測: 依存キャッシュ
済みでフルビルド 144 秒)。ソースを ext4 に別クローンする必要はない。

worktree の `.git` は Windows パスを指すファイルのため、**WSL から git は実行できない**。
編集と git は Windows 側、ビルドとテストは WSL 側という分担になる。
