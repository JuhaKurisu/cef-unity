# Linux サポート フェーズ 2 — Unity 向け配置と CI 設計書

作成日: 2026-07-29

前提: `2026-07-29-linux-support-phase1-design.md` (Rust 単体 + Harness の Linux 動作) が完了済み。

## 到達ライン

- `Assets/CefUnity/Plugins/linux-x64/` に Unity 向けの Linux x64 プラグイン一式が揃う
- Linux arm64 は CI でネイティブにビルド・実行検証され、GitHub Release の成果物として出る
- CI (`rust-build.yml`) に Linux ジョブが追加され、x64 / arm64 の双方が緑になる

## 前提調査の結果

| 確認項目 | 結果 |
|---|---|
| CEF の Linux arm64 配布物 | **存在する。** `download-cef` が `aarch64-unknown-linux-gnu` → `linuxarm64` を解決する (`download-cef-2.3.0/src/lib.rs:184`) |
| GitHub Actions の arm64 Linux ランナー | **無料で使える。** リポジトリが PUBLIC のため `ubuntu-24.04-arm` が利用可能 |
| Unity の Linux arm64 対応 | **存在しない。** Unity 6000.3.8f1 のデスクトップ Linux ターゲットは `StandaloneLinux64` (x86_64) のみ。arm64 は `EmbeddedLinux` という別ライセンスのプラットフォーム扱いで、Editor 自体も x86_64 のみ |
| `generate-missing-meta.sh` | **Windows 決め打ち。** `plugin_cpu_for()` が `Plugins/win-x64/*.dll` と `win-arm64/*.dll` にしかマッチせず、テンプレートも `Exclude Linux64: 1` / `Win64: enabled: 1` / `Editor OS: Windows` を固定で書く |
| 既存 `.meta` のプラットフォームキー | `Any` / `Editor` / `Linux64` / `OSXUniversal` / `Win` / `Win64` |

## アーキテクチャ判断

### arm64 成果物は `Assets/` に置かない

Unity に Linux arm64 のターゲットが無いため、`Plugins/linux-arm64/` を作っても全プラットフォームで
無効な約 1 GB の LFS 死荷重にしかならない。arm64 は CI でビルド・検証し、
GitHub Release の zip としてのみ配布する。非 Unity ホスト (`CefUnity.Viewer`、arm64 サーバー等)
からは利用できる。

Unity が将来デスクトップ Linux arm64 に対応した場合は、CI が既に成果物を作っているので
`publish` ジョブに配置ステップを 1 つ足すだけで済む。

### 配置スクリプトは Windows の構造を踏襲する

Windows は `deploy.ps1` (Unity 配置) と `CefUnity.Viewer.csproj` (ビルド出力) の双方が
共有スクリプト `copy-windows-runtime.ps1` を呼ぶ構造になっている。Linux も同じ形にする。

現状 `build-server-sandbox.sh` の Linux 分岐がコピー処理を内蔵しているが、`deploy-linux.sh` と
重複するため共有スクリプトへ切り出す。

## 変更点

### 1. `cef-unity-rust/copy-linux-runtime.sh` (新規)

Rust 成果物と CEF ランタイムを指定ディレクトリへフラット配置する。`copy-windows-runtime.ps1` の
Linux 版。

- 呼び出し形式: `copy-linux-runtime.sh <配置先ディレクトリ> <debug|release>`
  (第 2 引数は省略時 `release`)
- CEF 配布物の探索はワイルドカード `cef_linux_*` (x86_64 / aarch64 の双方で通る)
- **Rust 成果物 3 つ**: `cef-unity-server`, `cef-unity-rust-helper`, `libcef_unity_rust.so`
  (`copy-windows-runtime.ps1` が dll 3 つを配置するのと同じ構成)
- **CEF ランタイム**: `libcef.so`, `libEGL.so`, `libGLESv2.so`, `libvk_swiftshader.so`,
  `libvulkan.so.1`
- **リソース**: `vk_swiftshader_icd.json`, `icudtl.dat`, `v8_context_snapshot.bin`,
  `resources.pak`, `chrome_100_percent.pak`, `chrome_200_percent.pak`, `locales/`
  (Windows にある `snapshot_blob.bin` は Linux 配布物に存在しない)
- `chrome-sandbox` は `settings.no_sandbox = 1` (`server.rs:957`) のため配置しない
- ソースが無い場合はエラーにせず警告のみ (`copy-windows-runtime.ps1` と同じ方針。
  Rust 未ビルド環境で `dotnet build` を壊さないため)

**フェーズ 1 からの挙動変化に注意:** フェーズ 1 の `build-server-sandbox.sh` は
`libcef_unity_rust.so` を配置していなかった (Harness の csproj が `<None>` 項目で
出力ディレクトリへコピーしていたため)。共有スクリプト化により Harness の出力先にも
`.so` が配置されるようになるが、同じファイルなので実害は無い。csproj の `<None>` 項目は
**残す** — ステージングスクリプトが走らない経路でも `.so` が確実に置かれるようにするため。

### 2. `cef-unity-rust/deploy-linux.sh` (新規)

`cargo build --release` → `copy-linux-runtime.sh` → `Assets/CefUnity/Plugins/linux-x64/`。

- **ホストアーキのみ対応。** `uname -m` が `x86_64` でなければ、`linux-x64/` に誤った成果物を
  書き込む前にエラーで終了する
- `locales/` の Unity `.meta` を退避・復元する (`deploy.ps1` と同じ処理)
- 配置後、必須ファイル (`libcef_unity_rust.so`, `cef-unity-server`, `libcef.so`) の存在を検査する

### 3. `cef-unity-rust/build-server-sandbox.sh` (変更)

Linux 分岐のコピー処理を `copy-linux-runtime.sh` の呼び出しに置き換える。macOS 分岐には
一切触れない。フェーズ 1 で検証済みの挙動 (Harness の出力ディレクトリに debug 成果物が揃う)
を変えないこと。

### 4. `cef-unity-unityproject/generate-missing-meta.sh` (変更)

**このフェーズの実質的な本丸。** 現状は Windows プラグインしか正しく扱えない。

- `plugin_cpu_for()` を、CPU だけでなく**プラットフォームも返す**形に拡張し、
  `*/Plugins/linux-x64/*.so` を追加する
- `write_plugin_importer()` のテンプレートをプラットフォームで分岐させる:
  - Windows: `Win64: enabled: 1` / `Exclude Win64: 0` / `Exclude Linux64: 1` / `Editor OS: Windows`
  - Linux: `Linux64: enabled: 1` / `Exclude Linux64: 0` / `Exclude Win64: 1` / `Editor OS: Linux`
- `Editor` を有効にするのは x86_64 のプラグインのみという既存方針は維持する

これを外すと Unity が `.so` を Windows プラグインとして扱い、Linux ビルドから静かに除外される。

### 5. `.github/workflows/rust-build.yml` (変更)

**`build-linux` ジョブを追加 (マトリクス):**

| arch | runner |
|---|---|
| x64 | `ubuntu-24.04` |
| arm64 | `ubuntu-24.04-arm` |

両アーキとも同じ手順を踏む (クロスビルドではなくネイティブなので分岐が不要):

1. CEF 実行時依存の apt パッケージを導入
2. `cargo test --workspace --lib --release`
3. `cargo build --release`
4. `dotnet test cef-unity-csharp/CefUnity.Tests`
5. **Harness スモーク** (`SMOKE_OK frames=N`, N > 0)
6. `copy-linux-runtime.sh` で成果物を集め、`cef-unity-linux-<arch>` として upload

**`publish` ジョブの変更:**

- `needs` に `build-linux` を追加
- 「Update Plugins (linux)」ステップを追加。**x64 のみ** `Plugins/linux-x64/` へ展開する
  (Windows のステップと同じく `rsync --exclude '*.meta'` で `.meta` を保持)
- リリースアセットに `cef-unity-linux-x64.zip` と `cef-unity-linux-arm64.zip` を追加

### 6. `cef-unity-rust/CLAUDE.md` (変更)

Linux 節に `deploy-linux.sh` の使い方を追記し、「フェーズ 1 では deploy スクリプトは無い」の
記述を更新する。サポート対象プラットフォーム表に Linux arm64 の扱い
(CI 検証 + リリース成果物のみ、Unity 非対応) を追記する。

## ヘッドレス問題への対処

CI ランナーには画面が無いため、Harness スモークが**フェーズ 1 で埋められなかった
「真のヘッドレス環境」の検証**になる。フェーズ 1 の検証は WSLg (X11 が見える状態) で
行ったため、ここが赤くなる可能性は現実にある。

**方針: `xvfb-run` で CI に仮想ディスプレイを与えるのではなく、`server.rs` の
`on_before_command_line_processing` に `#[cfg(target_os = "linux")]` で
`--ozone-platform=headless` を追加する。**

理由: OSR にはウィンドウが無く、画面を要求する理由がない。CI だけ緑にする対処では、
実際のヘッドレス環境 (サーバー、コンテナ) のユーザーが救われない。

追加位置は `server.rs:777` の `disable-gpu-sandbox` の直後。既に CPU モードで
`disable-gpu` / `disable-gpu-compositing` が付く (`server.rs:779-784`) ので、これらを
重複して追加しないこと。

スモークが最初から通る場合はスイッチを追加しない。**先に素で試し、失敗したら追加する。**

## 検証手順

1. `bash cef-unity-rust/deploy-linux.sh` — `Plugins/linux-x64/` に成果物一式が揃い、
   `ldd` に `not found` が無い
2. `bash cef-unity-unityproject/generate-missing-meta.sh --check cef-unity-unityproject/Assets/CefUnity/Plugins`
   — `.meta` が全ファイルに揃っている
3. 生成された `.so` の `.meta` が `Linux64: enabled: 1` と `Exclude Linux64: 0` を持つ
4. `bash cef-unity-rust/build-server-sandbox.sh <dir>` — フェーズ 1 と同じ結果 (リファクタで壊していない)
5. CI が x64 / arm64 の双方で緑になり、`SMOKE_OK frames=N` (N > 0) が出る

5 番が本フェーズの合否判定となる。**arm64 と真のヘッドレス環境はローカルで検証できないため、
1-4 をローカルで通したうえでブランチを push し、CI の結果を見て初めて判定できる。**
CI が赤い場合の対処 (ヘッドレス対応) はこのフェーズの作業に含む。

このため実装は「ローカルで完結するタスク群」→「push して CI の結果を見るタスク」という
二段構えになる。CI の反復は 1 回あたり数分から十数分かかる (CEF の約 1 GB ダウンロードを含む)
ことを見込んでおくこと。

## 未知のリスク

- **ヘッドレス環境での CEF 初期化** — 上記の対処方針で解消する見込みだが、
  `--ozone-platform=headless` で OSR が正しく動くかは未検証
- **arm64 ランナーでの CEF 動作** — CEF の linuxarm64 配布物を実際に動かした実績が無い。
  SwiftShader (software GL) が arm64 で期待どおり動くかを含めて未知
- **Unity の Linux プラグイン `.meta` の正しさ** — Unity Editor for Linux の実機が無いため、
  生成した `.meta` が実際に Unity に正しく解釈されるかは目視での妥当性確認に留まる

## スコープ外

- Unity Editor for Linux での動作確認 (実機が無い)
- GPU ゼロコピー経路 (dmabuf / EGL)
- ネイティブ音声出力 (Linux は Unity ミキサ経路のみ)
- Linux arm64 の Unity 対応 (ターゲット自体が存在しない)

## 開発環境

フェーズ 1 と同じ。WSL2 (Ubuntu 24.04)、git worktree
`F:\GitHub\cef-unity\.claude\worktrees\linux-phase2` (ブランチ `worktree-linux-phase2`)、
`CARGO_TARGET_DIR` は ext4 側。編集と git は Windows 側、ビルドとテストは WSL 側。
