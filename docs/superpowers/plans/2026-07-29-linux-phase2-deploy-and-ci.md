# Linux サポート フェーズ 2 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `Assets/CefUnity/Plugins/linux-x64/` に Unity 向けの Linux x64 プラグインを配置できるようにし、Linux arm64 を CI でネイティブにビルド・実行検証して GitHub Release の成果物として出す。

**Architecture:** Windows の構造 (`deploy.ps1` + 共有の `copy-windows-runtime.ps1`) を Linux に写す。CI は x64/arm64 のマトリクスでネイティブランナーを使い、クロスビルドはしない。arm64 は Unity にターゲットが無いため `Assets/` には置かず、リリース成果物のみとする。

**Tech Stack:** Bash / GitHub Actions / Unity 6000.3.8f1 の PluginImporter meta / Rust 2024 / .NET 10

設計書: `docs/superpowers/specs/2026-07-29-linux-phase2-deploy-and-ci-design.md`

## Global Constraints

- **命名規約**: 識別子は省略形を使わずフルネーム。**シェル変数も同様** (`$CEF_DIRECTORY` であって `$CEF_DIR` ではない)。維持してよいのは辞書化された語 (app, config, info, max, min) と普遍的な頭字語 (id, url, gpu, cef, ipc, cpu, os)
- **アーキテクチャ名をハードコードしない**: CEF 配布物の探索はワイルドカード `cef_linux_*` を使う (既存 `cef_macos_*` / `cef_windows_*` と同じ方式)
- **既存の macOS / Windows 経路の挙動を変えない**: `build-server-sandbox.sh` の macOS 分岐、`deploy.ps1`、`copy-windows-runtime.ps1`、CI の `build-mac` / `build-win` ジョブには触れない
- **Intel Mac (x86_64 macOS) はサポートしない**
- **Linux arm64 を `Assets/` に置かない**: Unity にデスクトップ Linux arm64 のターゲットが存在しないため。arm64 は CI 検証とリリース zip のみ
- **`Plugins/` 配下のバイナリをこのブランチでコミットしない**: 約 1 GB の LFS コミットになる。バイナリと `.meta` は CI の `publish` ジョブがタグ時にコミットする。ローカルの `deploy-linux.sh` 実行結果は**未コミットのまま残す**
- 文字列リテラルは挙動契約なので変更しない: dev トグル、プロトコル文字列、CLI 引数、環境変数名
- コミットに Claude の属性 (Co-Authored-By trailer 等) を入れない
- **作業場所**: git worktree `F:\GitHub\cef-unity\.claude\worktrees\linux-phase2`
  (WSL からは `/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2`)。ブランチは
  `worktree-linux-phase2`。**main や他の worktree には一切触れない**
- **役割分担**:
  - **ファイル編集**は Windows 側のツール (Edit / Write) で行う
  - **ビルドとテスト**は WSL で実行する
  - **git 操作は Windows 側から行う。** worktree の `.git` は Windows パス (`F:/...`) を指す
    ファイルなので、WSL から git を実行すると `fatal: not a git repository` になる
- **WSL コマンドの実行方法**: 引用符の入れ子が壊れやすいため、複数行のコマンドは
  スクラッチパッドに `.sh` を書いてから渡す:
  `MSYS_NO_PATHCONV=1 wsl.exe -d Ubuntu-24.04 -- bash <スクリプトの /mnt/c/... パス>`
  (`MSYS_NO_PATHCONV=1` が無いと Git Bash が `/mnt/...` を Windows パスに変換して失敗する)。
  `/tmp` は wsl.exe の呼び出しをまたぐと消えることがあるので、複数ステップは 1 回にまとめる
- **ビルド時の環境変数**: `. "$HOME/.cargo/env"` / `export CARGO_TARGET_DIR="$HOME/cef-target-mnt"` /
  `export PATH="$HOME/.dotnet:$PATH"` / `export DOTNET_ROOT="$HOME/.dotnet"`

## File Structure

| ファイル | 区分 | 責務 |
|---|---|---|
| `cef-unity-rust/copy-linux-runtime.sh` | **新規** | Rust 成果物 + CEF ランタイムを指定ディレクトリへフラット配置 (debug/release 共用) |
| `cef-unity-rust/deploy-linux.sh` | **新規** | release ビルド → `Plugins/linux-x64/` へ配置。`.meta` 退避・復元 |
| `cef-unity-rust/build-server-sandbox.sh` | 変更 | Linux 分岐のコピー処理を `copy-linux-runtime.sh` 呼び出しに置換 |
| `cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes` | 変更 | `linux-x64/**` の LFS フィルタを追加 |
| `cef-unity-unityproject/generate-missing-meta.sh` | 変更 | PluginImporter をプラットフォーム対応にする (Win64 / Linux64) |
| `.github/workflows/rust-build.yml` | 変更 | `build-linux` ジョブ追加 + `publish` ジョブ更新 |
| `cef-unity-rust/CLAUDE.md` | 変更 | `deploy-linux.sh` の手順とサポート対象表を更新 |

---

### Task 1: `copy-linux-runtime.sh` の切り出し

`build-server-sandbox.sh` の Linux 分岐が持つコピー処理を、共有スクリプトへ切り出す。
Windows の `copy-windows-runtime.ps1` (deploy.ps1 と Viewer csproj の双方から呼ばれる) に対応する。

**Files:**
- Create: `cef-unity-rust/copy-linux-runtime.sh`
- Modify: `cef-unity-rust/build-server-sandbox.sh` (Linux 分岐のみ)

**Interfaces:**
- Consumes: なし
- Produces: `copy-linux-runtime.sh <destination_directory> [debug|release]`
  — 第 2 引数は省略時 `release`。配置に成功したら exit 0。
  Task 2 (deploy-linux.sh) と Task 4 (CI) がこれを呼ぶ

- [ ] **Step 1: 現状の挙動を記録する (リファクタの基準値)**

スクラッチパッドに以下を書いて実行する。

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE/cef-unity-rust" || exit 1
cargo build 2>&1 | tail -3
rm -rf "$HOME/stage-before" && mkdir -p "$HOME/stage-before"
bash build-server-sandbox.sh "$HOME/stage-before"
echo "=== ファイル一覧 (基準値) ==="
(cd "$HOME/stage-before" && find . -type f | sort | tee "$HOME/stage-before.txt" | wc -l)
```

Expected: `server staged (flat) at ...` に続いてファイル数が出る (`locales/` を含め 230 前後)。
この一覧が Step 4 の比較対象になる。

- [ ] **Step 2: `copy-linux-runtime.sh` を新規作成**

`cef-unity-rust/copy-linux-runtime.sh` を以下の内容で作成する。

```bash
#!/bin/bash
# Rust 成果物と CEF ランタイムを指定ディレクトリへフラット配置する (Linux)。
# deploy-linux.sh (Unity 配置) と build-server-sandbox.sh (Harness 出力) の双方から呼ばれる。
# copy-windows-runtime.ps1 の Linux 版。
#
# cef-unity-server は libcef_unity_rust.so と同じディレクトリ直下から起動されるため
# (crates/client/src/lib.rs の server_binary_path)、両者を同じ場所に置く必要がある。
#
# ソースが無い場合はエラーにせず警告のみ (Rust 未ビルド環境で dotnet build を壊さないため)。
# 成果物の欠落を検出したい呼び出し元は、呼ぶ前後で自前に検査すること。
#
# 使い方: copy-linux-runtime.sh <destination_directory> [debug|release]
set -u

DESTINATION_DIRECTORY="${1:-}"
BUILD_PROFILE="${2:-release}"
if [ -z "$DESTINATION_DIRECTORY" ]; then
    echo "使い方: $0 <destination_directory> [debug|release]" >&2
    exit 2
fi
if [ "$BUILD_PROFILE" != "debug" ] && [ "$BUILD_PROFILE" != "release" ]; then
    echo "ERROR: ビルド構成は debug か release: $BUILD_PROFILE" >&2
    exit 2
fi

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"
# CARGO_TARGET_DIR が設定されていればそちらを使う (ローカル開発で target/ を
# 別ファイルシステムへ逃がしている場合に対応する)。
TARGET_DIRECTORY="${CARGO_TARGET_DIR:-$SCRIPT_DIRECTORY/target}/$BUILD_PROFILE"

if [ ! -d "$TARGET_DIRECTORY" ]; then
    echo "[copy-linux-runtime] $TARGET_DIRECTORY が無いのでスキップ" >&2
    exit 0
fi

mkdir -p "$DESTINATION_DIRECTORY"

# ---- Rust 成果物 ----
for artifact in cef-unity-server cef-unity-rust-helper libcef_unity_rust.so; do
    if [ -f "$TARGET_DIRECTORY/$artifact" ]; then
        cp "$TARGET_DIRECTORY/$artifact" "$DESTINATION_DIRECTORY/"
    else
        echo "[copy-linux-runtime] missing artifact (skipped): $artifact" >&2
    fi
done

# ---- CEF ランタイムを cef-dll-sys のビルド出力から拾う ----
# アーキテクチャ名はワイルドカードで受ける (x86_64 / aarch64 の双方で通る)。
CEF_DIRECTORY=$(ls -d "$TARGET_DIRECTORY/build/cef-dll-sys-"*/out/cef_linux_* 2>/dev/null | head -1)
if [ -z "$CEF_DIRECTORY" ]; then
    echo "[copy-linux-runtime] CEF ランタイムが見つからないのでスキップ" >&2
    exit 0
fi

# 共有ライブラリ (Chromium / Angle / SwiftShader / Vulkan)。
# chrome-sandbox は settings.no_sandbox = 1 のため配置しない。
for library in libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1; do
    if [ -f "$CEF_DIRECTORY/$library" ]; then
        cp "$CEF_DIRECTORY/$library" "$DESTINATION_DIRECTORY/"
    else
        echo "[copy-linux-runtime] missing runtime library (skipped): $library" >&2
    fi
done

# リソース (V8 snapshot / ICU / pak / SwiftShader manifest)。
# Windows にある snapshot_blob.bin は Linux 配布物には存在しない。
for resource in icudtl.dat v8_context_snapshot.bin resources.pak \
                chrome_100_percent.pak chrome_200_percent.pak vk_swiftshader_icd.json; do
    if [ -f "$CEF_DIRECTORY/$resource" ]; then
        cp "$CEF_DIRECTORY/$resource" "$DESTINATION_DIRECTORY/"
    fi
done

# locales/ は呼び出し元が .meta を保持したい場合、呼ぶ前後で自前に退避・復元すること。
if [ -d "$CEF_DIRECTORY/locales" ]; then
    rm -rf "$DESTINATION_DIRECTORY/locales"
    cp -r "$CEF_DIRECTORY/locales" "$DESTINATION_DIRECTORY/locales"
fi

echo "[copy-linux-runtime] done -> $DESTINATION_DIRECTORY"
```

- [ ] **Step 3: `build-server-sandbox.sh` の Linux 分岐を置き換える**

`cef-unity-rust/build-server-sandbox.sh` の 14-51 行 (`# --- Linux:` のコメントから
`fi` まで) を以下で置き換える。macOS 側 (53 行目以降) には**一切触れない**。

置換前 (`# --- Linux:` の行から、対応する `fi` まで全体):

```bash
# --- Linux: バンドル概念が無いのでフラット配置する ---
# macOS 側の処理 (以降) は変更しない。Linux はここで配置を終えて exit する。
if [ "$(uname -s)" = "Linux" ]; then
    # CEF 配布物ディレクトリ。アーキテクチャ名はワイルドカードで受ける
    # (macOS 側の cef_macos_* と同じ方式。x86_64 / aarch64 の双方で通る)。
    CEF_DIRECTORY=$(ls -d "$SCRIPT_DIR/target/debug/build/cef-dll-sys-"*/out/cef_linux_* 2>/dev/null | head -1)
    if [ -z "$CEF_DIRECTORY" ]; then
        echo "ERROR: CEF build output not found. Run 'cargo build' first."
        exit 1
    fi

    mkdir -p "$OUTPUT_DIR"

    # Rust 成果物。server は client の dylib と同じディレクトリ直下から起動される
    # (crates/client/src/lib.rs の server_binary_path)。
    for artifact in cef-unity-server cef-unity-rust-helper; do
        cp "$SCRIPT_DIR/target/debug/$artifact" "$OUTPUT_DIR/"
    done

    # CEF ランタイム共有ライブラリ (Chromium / Angle / SwiftShader / Vulkan)。
    # chrome-sandbox は settings.no_sandbox = 1 のため配置しない。
    for library in libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1; do
        cp "$CEF_DIRECTORY/$library" "$OUTPUT_DIR/"
    done

    # リソース (V8 snapshot / ICU / pak / SwiftShader manifest)。
    # Windows にある snapshot_blob.bin は Linux 配布物には存在しない。
    for resource in icudtl.dat v8_context_snapshot.bin resources.pak \
                    chrome_100_percent.pak chrome_200_percent.pak vk_swiftshader_icd.json; do
        cp "$CEF_DIRECTORY/$resource" "$OUTPUT_DIR/"
    done

    rm -rf "$OUTPUT_DIR/locales"
    cp -r "$CEF_DIRECTORY/locales" "$OUTPUT_DIR/locales"

    echo "server staged (flat) at $OUTPUT_DIR"
    exit 0
fi
```

置換後:

```bash
# --- Linux: バンドル概念が無いのでフラット配置する ---
# 配置処理は deploy-linux.sh と共有する (copy-windows-runtime.ps1 と同じ構造)。
# macOS 側の処理 (以降) は変更しない。Linux はここで配置を終えて exit する。
if [ "$(uname -s)" = "Linux" ]; then
    bash "$SCRIPT_DIR/copy-linux-runtime.sh" "$OUTPUT_DIR" debug
    echo "server staged (flat) at $OUTPUT_DIR"
    exit 0
fi
```

- [ ] **Step 4: リファクタ前後で配置物が変わっていないことを確認**

スクラッチパッドに以下を書いて実行する。

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE/cef-unity-rust" || exit 1
rm -rf "$HOME/stage-after" && mkdir -p "$HOME/stage-after"
bash build-server-sandbox.sh "$HOME/stage-after"
(cd "$HOME/stage-after" && find . -type f | sort > "$HOME/stage-after.txt")
echo "=== 差分 (基準値 vs リファクタ後) ==="
diff "$HOME/stage-before.txt" "$HOME/stage-after.txt" && echo "差分なし"
echo "=== ldd (未解決が無いこと) ==="
ldd "$HOME/stage-after/cef-unity-server" | grep "not found" || echo "ALL_RESOLVED"
```

Expected: 差分は `./libcef_unity_rust.so` の **1 行追加のみ** (`> ./libcef_unity_rust.so`)。
これは意図した挙動変化 (設計書「フェーズ 1 からの挙動変化に注意」)。
続いて `ALL_RESOLVED` が出る。

これ以外の差分が出た場合はリファクタが挙動を変えている。原因を特定して直すこと。

- [ ] **Step 5: Harness が引き続きビルド・実行できることを確認**

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
export PATH="$HOME/.dotnet:$PATH"
export DOTNET_ROOT="$HOME/.dotnet"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE" || exit 1
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -3
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0 || exit 1
./CefUnity.Harness smoke 2>&1 | tail -3
```

Expected: `Build succeeded` に続いて `SMOKE_OK frames=N` (N > 0)。
フェーズ 1 で確認した経路がリファクタで壊れていないことの確認。

- [ ] **Step 6: コミット**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add cef-unity-rust/copy-linux-runtime.sh cef-unity-rust/build-server-sandbox.sh
git commit -m "refactor(linux): 配置処理を copy-linux-runtime.sh に切り出して deploy と共有する"
```

---

### Task 2: `deploy-linux.sh` と `.gitattributes`

`deploy.ps1` の Linux 版。release ビルドを `Assets/CefUnity/Plugins/linux-x64/` へ配置する。

**Files:**
- Create: `cef-unity-rust/deploy-linux.sh`
- Modify: `cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes`

**Interfaces:**
- Consumes: `copy-linux-runtime.sh <destination_directory> [debug|release]` (Task 1)
- Produces: `bash cef-unity-rust/deploy-linux.sh` — 引数なし。ホストが x86_64 でなければエラー終了

- [ ] **Step 1: `.gitattributes` に LFS フィルタを追加**

`cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes` を編集する。

置換前:

```
osx-arm64/** filter=lfs diff=lfs merge=lfs -text
win-x64/**   filter=lfs diff=lfs merge=lfs -text
win-arm64/** filter=lfs diff=lfs merge=lfs -text
```

置換後 (行を 1 つ追加するだけ。既存 3 行は変更しない):

```
osx-arm64/** filter=lfs diff=lfs merge=lfs -text
win-x64/**   filter=lfs diff=lfs merge=lfs -text
win-arm64/** filter=lfs diff=lfs merge=lfs -text
linux-x64/** filter=lfs diff=lfs merge=lfs -text
```

`**/*.meta -filter -diff -merge text` の行はそのまま残す (`.meta` は平文で管理する)。

- [ ] **Step 2: `deploy-linux.sh` を新規作成**

`cef-unity-rust/deploy-linux.sh` を以下の内容で作成する。

```bash
#!/bin/bash
# Linux x86_64 用のビルド + Unity プラグインへのコピー。
# libcef_unity_rust.so (cdylib), cef-unity-server, cef-unity-rust-helper,
# および CEF ランタイム (libcef.so, *.pak, *.dat, *.bin, locales/ 等) を一括配置する。
#
# ホストアーキテクチャ向けにしかビルドしない。Unity にはデスクトップ Linux arm64 の
# ターゲットが無いため、arm64 の成果物は CI がリリース zip として出す (Assets には置かない)。
set -e

SCRIPT_DIRECTORY="$(cd "$(dirname "$0")" && pwd)"

HOST_ARCHITECTURE="$(uname -m)"
if [ "$HOST_ARCHITECTURE" != "x86_64" ]; then
    echo "ERROR: deploy-linux.sh は x86_64 ホスト専用です (検出: $HOST_ARCHITECTURE)。" >&2
    echo "       Plugins/linux-x64/ に誤ったアーキテクチャの成果物を置かないため中止します。" >&2
    exit 1
fi

DESTINATION_DIRECTORY="$SCRIPT_DIRECTORY/../cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64"

echo "[deploy-linux] cargo build --release"
cd "$SCRIPT_DIRECTORY"
cargo build --release

mkdir -p "$DESTINATION_DIRECTORY"

# ---- locales/ の Unity .meta を退避 ----
# 共有スクリプトは .meta を関知しないため、退避・復元は Unity 配置固有の処理として
# ここで行う。旧ファイルの残留を避けるため共有スクリプトが locales をディレクトリごと
# 作り直すので、その前後で .meta を保存する。
LOCALES_DESTINATION="$DESTINATION_DIRECTORY/locales"
META_TEMPORARY=""
if [ -d "$LOCALES_DESTINATION" ]; then
    META_TEMPORARY="$(mktemp -d)"
    find "$LOCALES_DESTINATION" -maxdepth 1 -name '*.meta' -exec cp {} "$META_TEMPORARY/" \; 2>/dev/null || true
fi

bash "$SCRIPT_DIRECTORY/copy-linux-runtime.sh" "$DESTINATION_DIRECTORY" release

# ---- 退避した .meta を復元 ----
if [ -n "$META_TEMPORARY" ] && [ -d "$META_TEMPORARY" ]; then
    find "$META_TEMPORARY" -maxdepth 1 -name '*.meta' -exec cp {} "$LOCALES_DESTINATION/" \; 2>/dev/null || true
    rm -rf "$META_TEMPORARY"
fi

# ---- 成果物の欠落を検査する ----
# 共有スクリプトは欠落を警告のみで通すため、Unity 配置では厳格に検査する。
for required in libcef_unity_rust.so cef-unity-server cef-unity-rust-helper libcef.so; do
    if [ ! -f "$DESTINATION_DIRECTORY/$required" ]; then
        echo "ERROR: deploy failed: $required が $DESTINATION_DIRECTORY にコピーされていません" >&2
        exit 1
    fi
done

echo "[deploy-linux] done -> $DESTINATION_DIRECTORY"
```

- [ ] **Step 3: 実行して配置物が揃うことを確認**

release ビルドは CEF の再ダウンロードを含むため時間がかかる (10 分程度見込む)。

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
bash "$WORKTREE/cef-unity-rust/deploy-linux.sh"
echo "=== 配置物の検査 ==="
DESTINATION="$WORKTREE/cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64"
for required in libcef_unity_rust.so cef-unity-server cef-unity-rust-helper \
                libcef.so icudtl.dat resources.pak v8_context_snapshot.bin; do
    test -e "$DESTINATION/$required" && echo "ok   $required" || echo "MISSING $required"
done
test -d "$DESTINATION/locales" && echo "ok   locales/ ($(ls "$DESTINATION/locales" | wc -l) files)" || echo "MISSING locales/"
echo "=== ldd ==="
ldd "$DESTINATION/cef-unity-server" | grep "not found" || echo "ALL_RESOLVED"
```

Expected: すべて `ok`、`locales/` が 220 ファイル、`ALL_RESOLVED`。

- [ ] **Step 4: バイナリをコミットしないことを確認**

```bash
git status --short | head -20
git status --short -- cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64 | wc -l
```

Expected: `linux-x64/` 配下に多数の未追跡ファイルが見えるが、**これらはステージしない**。
約 1 GB の LFS コミットになるため。バイナリと `.meta` は CI の `publish` ジョブが
タグ時にコミットする。

- [ ] **Step 5: コミット (スクリプトと .gitattributes のみ)**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add cef-unity-rust/deploy-linux.sh cef-unity-unityproject/Assets/CefUnity/Plugins/.gitattributes
git commit -m "feat(linux): deploy-linux.sh を追加し linux-x64 を LFS 追跡対象にする"
git status --short -- cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64 | head -3
```

Expected: コミット後も `linux-x64/` 配下は未追跡のまま残る (`??` で表示される)。

---

### Task 3: `generate-missing-meta.sh` のプラットフォーム対応

**このフェーズの本丸。** 現状 `plugin_cpu_for()` は Windows の dll にしかマッチせず、
`write_plugin_importer()` は `Exclude Linux64: 1` / `Win64: enabled: 1` / `Editor OS: Windows` を
固定で書く。Linux の `.so` に正しいインポーター設定が付かないと、Unity が `.so` を
Windows プラグインとして扱い Linux ビルドから静かに除外する。

**Files:**
- Modify: `cef-unity-unityproject/generate-missing-meta.sh`

**Interfaces:**
- Consumes: なし
- Produces: `generate-missing-meta.sh [--check] <ディレクトリ>` (シグネチャは変更しない)。
  `Plugins/linux-x64/*.so` に `Linux64: enabled: 1` を持つ PluginImporter meta を生成する

- [ ] **Step 1: 失敗するテストを書く**

`.meta` の生成結果を検証するフィクスチャベースのテストを作る。スクラッチパッドに
`test-meta-generation.sh` として以下を書く (リポジトリには置かない — 検証専用)。

```bash
#!/bin/bash
# generate-missing-meta.sh が Linux プラグインに正しい PluginImporter を書くかを検証する。
set -u
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
FIXTURE="$HOME/meta-fixture"

rm -rf "$FIXTURE"
mkdir -p "$FIXTURE/Assets/CefUnity/Plugins/linux-x64"
mkdir -p "$FIXTURE/Assets/CefUnity/Plugins/win-x64"
mkdir -p "$FIXTURE/ProjectSettings"
echo "dummy" > "$FIXTURE/Assets/CefUnity/Plugins/linux-x64/libcef.so"
echo "dummy" > "$FIXTURE/Assets/CefUnity/Plugins/win-x64/libcef.dll"

bash "$WORKTREE/cef-unity-unityproject/generate-missing-meta.sh" "$FIXTURE/Assets/CefUnity/Plugins" >/dev/null

failures=0
assert_contains() {
    local file="$1" pattern="$2" label="$3"
    if grep -q "$pattern" "$file"; then
        echo "ok   $label"
    else
        echo "FAIL $label — '$pattern' が $file に無い"
        failures=$((failures + 1))
    fi
}

LINUX_META="$FIXTURE/Assets/CefUnity/Plugins/linux-x64/libcef.so.meta"
WINDOWS_META="$FIXTURE/Assets/CefUnity/Plugins/win-x64/libcef.dll.meta"

echo "=== Linux プラグインの meta ==="
assert_contains "$LINUX_META" "PluginImporter:" "PluginImporter が書かれている"
assert_contains "$LINUX_META" "Exclude Linux64: 0" "Linux64 が除外されていない"
assert_contains "$LINUX_META" "Exclude Win64: 1" "Win64 が除外されている"
assert_contains "$LINUX_META" "OS: Linux" "Editor の OS が Linux"
# Linux64 ブロックが enabled: 1 と CPU: x86_64 を持つこと
if awk '/^    Linux64:/{f=1} f&&/enabled: 1/{print "enabled"; exit}' "$LINUX_META" | grep -q enabled; then
    echo "ok   Linux64 が enabled: 1"
else
    echo "FAIL Linux64 が enabled: 1 になっていない"
    failures=$((failures + 1))
fi

echo "=== Windows プラグインの meta (既存挙動が壊れていないこと) ==="
assert_contains "$WINDOWS_META" "Exclude Win64: 0" "Win64 が除外されていない"
assert_contains "$WINDOWS_META" "Exclude Linux64: 1" "Linux64 が除外されている"
assert_contains "$WINDOWS_META" "OS: Windows" "Editor の OS が Windows"

echo
if [ "$failures" -eq 0 ]; then echo "ALL_PASS"; else echo "FAILURES=$failures"; fi
```

- [ ] **Step 2: テストが失敗することを確認**

```
MSYS_NO_PATHCONV=1 wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/.../scratchpad/test-meta-generation.sh
```

Expected: FAIL。Linux 側の 5 項目が `FAIL` になる (現状は `.so` に PluginImporter が
書かれないため、`libcef.so.meta` は `fileFormatVersion` と `guid` のみ)。
Windows 側の 3 項目は `ok`。

- [ ] **Step 3: `plugin_cpu_for()` をプラットフォーム対応に置き換える**

`cef-unity-unityproject/generate-missing-meta.sh` の 62-73 行付近を置き換える。

置換前:

```bash
# win-x64 と win-arm64 には同名の dll (libcef.dll 等) が並ぶ。インポーター設定が
# 無いと Unity が両方を同じプラットフォーム向けと見なして "同名プラグインが複数ある"
# と衝突扱いにするため、ネイティブ dll には CPU を明示した PluginImporter を書く。
#
# Editor は x64 のみ有効にする (x64 Editor が ARM64 の dll を掴まないようにするため。
# ARM64 の Unity Editor で開発する場合はここを見直す必要がある)。
plugin_cpu_for() {
    local relative_path="$1"
    case "$relative_path" in
        */Plugins/win-x64/*.dll) echo "x86_64" ;;
        */Plugins/win-arm64/*.dll) echo "ARM64" ;;
        *) echo "" ;;
    esac
}
```

置換後:

```bash
# win-x64 と win-arm64 には同名の dll (libcef.dll 等) が並ぶ。インポーター設定が
# 無いと Unity が両方を同じプラットフォーム向けと見なして "同名プラグインが複数ある"
# と衝突扱いにするため、ネイティブ dll には CPU を明示した PluginImporter を書く。
# Linux の .so も同様に、Windows プラグインと誤認されないよう明示が要る。
#
# Editor は x64 のみ有効にする (x64 Editor が ARM64 の dll を掴まないようにするため。
# ARM64 の Unity Editor で開発する場合はここを見直す必要がある)。
#
# 戻り値は "<Unity プラットフォームキー>:<CPU>"。該当しなければ空文字列。
# Unity にはデスクトップ Linux arm64 のターゲットが無いため linux-arm64 は扱わない。
plugin_platform_and_cpu_for() {
    local relative_path="$1"
    case "$relative_path" in
        */Plugins/win-x64/*.dll) echo "Win64:x86_64" ;;
        */Plugins/win-arm64/*.dll) echo "Win64:ARM64" ;;
        */Plugins/linux-x64/*.so) echo "Linux64:x86_64" ;;
        *) echo "" ;;
    esac
}
```

- [ ] **Step 4: `write_plugin_importer()` をプラットフォーム分岐させる**

同ファイルの `write_plugin_importer()` 全体 (`write_plugin_importer() {` から
対応する `}` まで) を以下で置き換える。

```bash
write_plugin_importer() {
    local platform="$1"
    local cpu="$2"

    local editor_enabled=0
    local editor_cpu="None"
    local editor_os="Windows"
    local windows_enabled=0
    local windows_cpu="None"
    local linux_enabled=0
    local linux_cpu="None"
    local exclude_windows=1
    local exclude_linux=1

    if [ "$platform" = "Linux64" ]; then
        linux_enabled=1
        linux_cpu="$cpu"
        exclude_linux=0
        editor_os="Linux"
    else
        windows_enabled=1
        windows_cpu="$cpu"
        exclude_windows=0
        editor_os="Windows"
    fi

    if [ "$cpu" = "x86_64" ]; then
        editor_enabled=1
        editor_cpu="x86_64"
    fi

    cat <<EOF
PluginImporter:
  externalObjects: {}
  serializedVersion: 3
  iconMap: {}
  executionOrder: {}
  defineConstraints: []
  isPreloaded: 0
  isOverridable: 0
  isExplicitlyReferenced: 0
  validateReferences: 1
  platformData:
    Any:
      enabled: 0
      settings:
        Exclude Editor: $((1 - editor_enabled))
        Exclude Linux64: $exclude_linux
        Exclude OSXUniversal: 1
        Exclude Win: 1
        Exclude Win64: $exclude_windows
    Editor:
      enabled: $editor_enabled
      settings:
        CPU: $editor_cpu
        DefaultValueInitialized: true
        OS: $editor_os
    Linux64:
      enabled: $linux_enabled
      settings:
        CPU: $linux_cpu
    OSXUniversal:
      enabled: 0
      settings:
        CPU: None
    Win:
      enabled: 0
      settings:
        CPU: None
    Win64:
      enabled: $windows_enabled
      settings:
        CPU: $windows_cpu
  userData:
  assetBundleName:
  assetBundleVariant:
EOF
}
```

- [ ] **Step 5: 呼び出し側 2 箇所を新しいシグネチャに合わせる**

同ファイルの `write_meta()` 内を置き換える。

置換前:

```bash
    local cpu
    cpu="$(plugin_cpu_for "$relative_path")"
```

置換後:

```bash
    local platform_and_cpu
    platform_and_cpu="$(plugin_platform_and_cpu_for "$relative_path")"
```

同じ `write_meta()` 内の分岐を置き換える。

置換前:

```bash
        elif [ -n "$cpu" ]; then
            write_plugin_importer "$cpu"
        fi
```

置換後:

```bash
        elif [ -n "$platform_and_cpu" ]; then
            write_plugin_importer "${platform_and_cpu%%:*}" "${platform_and_cpu##*:}"
        fi
```

続いて `needs_plugin_importer()` を置き換える。

置換前:

```bash
    [ -n "$(plugin_cpu_for "$relative_path")" ] || return 1
```

置換後:

```bash
    [ -n "$(plugin_platform_and_cpu_for "$relative_path")" ] || return 1
```

- [ ] **Step 6: テストが通ることを確認**

```
MSYS_NO_PATHCONV=1 wsl.exe -d Ubuntu-24.04 -- bash /mnt/c/.../scratchpad/test-meta-generation.sh
```

Expected: PASS。`ALL_PASS` が出る (8 項目すべて `ok`)。

- [ ] **Step 7: 既存の Plugins に対して `--check` が通ることを確認**

既存の win-x64 / osx-arm64 の meta を壊していないことの確認。

```bash
#!/bin/bash
set -u
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE" || exit 1
bash cef-unity-unityproject/generate-missing-meta.sh --check \
     cef-unity-unityproject/Assets/CefUnity/Plugins
echo "exit=$?"
```

Expected: 既存 meta について不足も差し替え要求も出ないこと。
注意: Task 2 で `linux-x64/` にバイナリを配置済みの場合、それらの `.meta` が
無いため不足として報告される。これは想定内 (CI の publish が生成する)。
**既存の win-x64 / osx-arm64 について何も報告されないこと**を確認する。

- [ ] **Step 8: 生成された Linux meta を目視確認**

```bash
cat "$HOME/meta-fixture/Assets/CefUnity/Plugins/linux-x64/libcef.so.meta"
```

Expected: `Linux64:` ブロックが `enabled: 1` / `CPU: x86_64`、`Win64:` が
`enabled: 0` / `CPU: None`、`Editor` が `enabled: 1` / `CPU: x86_64` / `OS: Linux`。

- [ ] **Step 9: コミット**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add cef-unity-unityproject/generate-missing-meta.sh
git commit -m "feat(unity): .meta 生成を Linux プラグインに対応させる"
```

---

### Task 4: CI に `build-linux` ジョブを追加し `publish` を更新

**Files:**
- Modify: `.github/workflows/rust-build.yml`

**Interfaces:**
- Consumes: `copy-linux-runtime.sh <destination_directory> [debug|release]` (Task 1)
- Produces: アーティファクト `cef-unity-linux-x64` / `cef-unity-linux-arm64`。
  Task 5 がこれらの CI 実行結果を判定に使う

- [ ] **Step 1: `build-linux` ジョブを追加**

`.github/workflows/rust-build.yml` の `build-win` ジョブの直後 (`publish:` ジョブの
コメント行の直前) に以下を挿入する。`build-mac` / `build-win` には触れない。

```yaml
  build-linux:
    strategy:
      fail-fast: false
      matrix:
        include:
          # PUBLIC リポジトリなので arm64 ネイティブランナーが無料で使える。
          # クロスビルドではないので x64/arm64 で手順が分岐しない。
          - arch: x64
            runner: ubuntu-24.04
          - arch: arm64
            runner: ubuntu-24.04-arm
    runs-on: ${{ matrix.runner }}
    steps:
      - uses: actions/checkout@v4

      - uses: dtolnay/rust-toolchain@stable

      - name: Cache cargo registry
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
          key: cargo-linux-${{ matrix.arch }}-${{ hashFiles('cef-unity-rust/Cargo.lock') }}

      # CEF の実行時依存 (libcef.so が要求する共有ライブラリ)
      - name: Install CEF runtime dependencies
        run: |
          sudo apt-get update -qq
          sudo apt-get install -y -qq \
            libnss3 libnspr4 libasound2t64 libatk1.0-0t64 libatk-bridge2.0-0t64 \
            libcups2t64 libdrm2 libgbm1 libgtk-3-0t64 libxcomposite1 libxdamage1 \
            libxfixes3 libxkbcommon0 libxrandr2 libpango-1.0-0 libcairo2 libx11-xcb1 libxss1

      # 初回の cargo 実行は CEF (~1GB) ダウンロードを含むため 1 回リトライ
      - name: Unit tests
        working-directory: cef-unity-rust
        run: cargo test --workspace --lib --release || cargo test --workspace --lib --release

      - name: Build
        working-directory: cef-unity-rust
        run: cargo build --release

      - name: Setup .NET
        uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '10.0.x'

      - name: Core unit tests (no Unity)
        run: dotnet test cef-unity-csharp/CefUnity.Tests -c Release --logger "console;verbosity=minimal"

      # ランナーには画面が無い。ここが「真のヘッドレス環境」での初めての検証になる。
      #
      # dotnet publish で Harness を 1 ディレクトリにまとめ、そこへ copy-linux-runtime.sh で
      # ネイティブ成果物を重ねる。csproj の CopyServerApp と .so の <None> は
      # Exists(target/debug/...) 条件付きで、CI は release ビルドのみなので発火しない。
      - name: Harness smoke
        run: |
          dotnet publish cef-unity-csharp/CefUnity.Harness -c Release -o "$RUNNER_TEMP/harness"
          bash cef-unity-rust/copy-linux-runtime.sh "$RUNNER_TEMP/harness" release
          cd "$RUNNER_TEMP/harness"
          ./CefUnity.Harness smoke

      - name: Collect bundle
        working-directory: cef-unity-rust
        run: |
          bash copy-linux-runtime.sh bundle-linux release
          echo "=== bundle-linux ==="
          ls -laR bundle-linux > "$RUNNER_TEMP/bundle_list.txt"
          head -80 "$RUNNER_TEMP/bundle_list.txt"

      - uses: actions/upload-artifact@v4
        with:
          name: cef-unity-linux-${{ matrix.arch }}
          path: cef-unity-rust/bundle-linux
```

- [ ] **Step 2: `publish` ジョブの `needs` に `build-linux` を追加**

置換前:

```yaml
  publish:
    needs: [build-mac, build-win]
```

置換後:

```yaml
  publish:
    needs: [build-mac, build-win, build-linux]
```

- [ ] **Step 3: `publish` に Linux の配置ステップを追加**

`- name: Update Plugins (win, フラット展開)` ステップの直後に以下を挿入する。

```yaml
      # Linux は x64 のみ Unity に配置する。Unity にデスクトップ Linux arm64 の
      # ターゲットが無いため、arm64 はリリース zip のみ (Assets には置かない)。
      - name: Update Plugins (linux, フラット展開)
        run: |
          WORK_DIRECTORY=cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64
          SOURCE=artifacts/cef-unity-linux-x64
          if [ ! -d "$SOURCE" ]; then
            echo "SKIP (artifact not found): $SOURCE"
          else
            mkdir -p "$WORK_DIRECTORY"
            # .meta は Unity 管理なので保持
            rsync -a --exclude '*.meta' "$SOURCE/" "$WORK_DIRECTORY/"
          fi
```

- [ ] **Step 4: リリースアセットに Linux を追加**

`Create GitHub Release` ステップの `run:` 内を置き換える。

置換前:

```bash
          for ARCH in x64 arm64; do
            if [ -d "artifacts/cef-unity-windows-$ARCH" ]; then
              (cd "artifacts/cef-unity-windows-$ARCH" && zip -qry "../../cef-unity-windows-$ARCH.zip" .)
              ASSETS+=("cef-unity-windows-$ARCH.zip")
            fi
          done
```

置換後:

```bash
          for ARCH in x64 arm64; do
            if [ -d "artifacts/cef-unity-windows-$ARCH" ]; then
              (cd "artifacts/cef-unity-windows-$ARCH" && zip -qry "../../cef-unity-windows-$ARCH.zip" .)
              ASSETS+=("cef-unity-windows-$ARCH.zip")
            fi
            if [ -d "artifacts/cef-unity-linux-$ARCH" ]; then
              (cd "artifacts/cef-unity-linux-$ARCH" && zip -qry "../../cef-unity-linux-$ARCH.zip" .)
              ASSETS+=("cef-unity-linux-$ARCH.zip")
            fi
          done
```

- [ ] **Step 5: YAML の構文を検証する**

```bash
#!/bin/bash
set -u
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
python3 -c "
import yaml, sys
with open('$WORKTREE/.github/workflows/rust-build.yml') as handle:
    document = yaml.safe_load(handle)
print('jobs:', list(document['jobs'].keys()))
print('build-linux matrix:', document['jobs']['build-linux']['strategy']['matrix']['include'])
print('publish needs:', document['jobs']['publish']['needs'])
"
```

Expected: `jobs: ['build-mac', 'build-win', 'build-linux', 'publish']`、
matrix に x64/ubuntu-24.04 と arm64/ubuntu-24.04-arm、
`publish needs: ['build-mac', 'build-win', 'build-linux']`。

`ModuleNotFoundError: No module named 'yaml'` が出たら
`sudo apt-get install -y python3-yaml` を実行してから再試行する。

- [ ] **Step 6: コミット**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add .github/workflows/rust-build.yml
git commit -m "ci(linux): x64/arm64 のネイティブビルドジョブを追加し publish を更新"
```

---

### Task 5: CI で検証する — フェーズ 2 の合否判定

ここがフェーズ 2 の本体。**arm64 と真のヘッドレス環境はローカルで検証できないため、
push して CI の結果を見て初めて判定できる。**

**Files:**
- Modify (必要になった場合のみ): `cef-unity-rust/crates/server/src/server.rs:777` 付近

**Interfaces:**
- Consumes: Task 1-4 のすべて
- Produces: CI が x64 / arm64 の双方で緑になるという実証

- [ ] **Step 1: ブランチを push して CI を起動する**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git push -u origin worktree-linux-phase2
```

注意: `rust-build.yml` の `on.push.branches` は `[main]` のみだが、`pull_request` の
`paths` に `.github/workflows/rust-build.yml` と `cef-unity-rust/**` が含まれる。
push だけでは走らないため、**PR を作成して CI を起動する**。

```bash
gh pr create --base main --head worktree-linux-phase2 \
  --title "Linux サポート フェーズ 2: Unity 向け配置と CI" \
  --body "設計書: docs/superpowers/specs/2026-07-29-linux-phase2-deploy-and-ci-design.md

- linux-x64 の Unity 配置 (deploy-linux.sh + 共有の copy-linux-runtime.sh)
- .meta 生成のプラットフォーム対応
- CI に x64/arm64 のネイティブ Linux ジョブを追加

arm64 と真のヘッドレス環境はローカルで検証できないため、この PR の CI が合否判定になる。"
```

- [ ] **Step 2: CI の結果を確認する**

```bash
gh pr checks --watch
```

または個別に:

```bash
gh run list --branch worktree-linux-phase2 --limit 5
gh run view <run-id> --log-failed
```

Expected: `build-linux (x64)` と `build-linux (arm64)` の双方が成功し、
`Harness smoke` ステップの出力に `SMOKE_OK frames=N` (N > 0) が出る。

- [ ] **Step 3: 失敗した場合の対処**

**成功した場合はこのステップを飛ばす。**

想定される失敗と対処:

| 症状 | 原因 | 対処 |
|---|---|---|
| `Harness smoke` で CEF 初期化に失敗し、ログに `Unable to open X display` / `ozone` 関連 | ランナーに画面が無い | 下記 (A) の `ozone-platform=headless` を追加 |
| `SMOKE_OK frames=0` | フレームが来ない | 下記 (B) の切り分け |
| arm64 のみ失敗 | CEF の linuxarm64 配布物や SwiftShader の問題 | ログを添えて BLOCKED で報告する。推測でスイッチを足さない |

**(A) ozone-platform を headless にする**

`cef-unity-rust/crates/server/src/server.rs` の 777 行 (`disable-gpu-sandbox` を
append している行) の直後に挿入する。

```rust
                // Linux: OSR にはウィンドウが無く画面を要求する理由がないため、
                // headless バックエンドを指定する。X11 が無い環境 (CI ランナー、
                // コンテナ、サーバー) で初期化が失敗するのを防ぐ。
                #[cfg(target_os = "linux")]
                command_line.append_switch_with_value(
                    Some(&CefString::from("ozone-platform")),
                    Some(&CefString::from("headless")),
                );
```

**重複して追加しないこと:** CPU モード (`use_gpu = false`) では `disable-gpu` と
`disable-gpu-compositing` が既に付く (`server.rs:779-784`)。Harness の smoke は
`CefRuntime.Initialize(useGpu: false)` を使うのでこの経路に入る。

**(B) フレームが来ない場合の切り分け**

CI のログに出るサーバーログ (`/tmp/cef_unity_server.log`) を採取するステップを
一時的に足して原因を見る。`on_paint` が呼ばれているかを確認し、呼ばれていなければ
external BeginFrame 側の問題としてフェーズ 3 に送る (設計書の「未知のリスク」に記録)。

追加したスイッチは**必ず `#[cfg(target_os = "linux")]` で囲い**、macOS / Windows の
挙動を変えないこと。修正後は commit → push して CI を再実行する。

- [ ] **Step 4: ローカルでも回帰していないことを確認**

スイッチを追加した場合のみ実施する。追加していなければ飛ばす。

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
export PATH="$HOME/.dotnet:$PATH"
export DOTNET_ROOT="$HOME/.dotnet"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE/cef-unity-rust" || exit 1
cargo build 2>&1 | tail -3
cd "$WORKTREE" || exit 1
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -2
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0 || exit 1
./CefUnity.Harness smoke 2>&1 | tail -3
```

Expected: `SMOKE_OK frames=N` (N > 0)。WSLg (X11 あり) でも headless スイッチが
悪影響を与えていないことの確認。

- [ ] **Step 5: コミット (スイッチ追加が必要だった場合のみ)**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add cef-unity-rust/crates/server/src/server.rs
git commit -m "fix(linux): ヘッドレス環境向けに ozone-platform=headless を指定する"
git push
```

---

### Task 6: `cef-unity-rust/CLAUDE.md` の更新

**Files:**
- Modify: `cef-unity-rust/CLAUDE.md`

**Interfaces:**
- Consumes: Task 5 の結果 (実際に必要だったスイッチが確定していること)
- Produces: なし (ドキュメント)

- [ ] **Step 1: Linux 節の deploy 手順を追記する**

`#### Linux (x86_64)` 節の冒頭にある「フェーズ 1 時点では **Unity への deploy スクリプトは無い**。
Rust 単体と C# Harness までの対応。」という趣旨の記述を、以下で置き換える。
まず Read で実際の文言を確認してから編集すること。

```markdown
#### Linux

Unity への配置は `deploy-linux.sh` を使う。**x86_64 ホスト専用**で、他アーキでは
誤配置を防ぐためエラー終了する。

```bash
bash deploy-linux.sh
```

成果物は `cef-unity-unityproject/Assets/CefUnity/Plugins/linux-x64/` にフラット配置される
(`libcef_unity_rust.so`, `cef-unity-server`, `cef-unity-rust-helper`, `libcef.so`,
各種 `.pak` / `.dat` / `.bin`, `locales/`)。

配置処理は `copy-linux-runtime.sh` に切り出されており、`build-server-sandbox.sh`
(Harness の出力先) と共有している。
```

- [ ] **Step 2: サポート対象プラットフォーム表を更新する**

`## サポート対象プラットフォーム` の表を以下で置き換える。

```markdown
| プラットフォーム | 状態 |
|---|---|
| macOS arm64 | GPU ゼロコピー (IOSurface/Mach/Metal) |
| Windows x64 | GPU ゼロコピー (D3D11 共有テクスチャ + 共有 fence) |
| Windows arm64 | クロスビルドのみ (実行検証なし) |
| Linux x86_64 | software paint のみ。Unity 配置あり (`linux-x64`) |
| Linux arm64 | software paint のみ。**Unity 配置なし** — Unity にデスクトップ Linux arm64 の<br>ターゲットが存在しないため、CI 検証と GitHub Release の zip のみ |
| macOS x86_64 (Intel Mac) | **サポートしない** |

`deploy.sh` の配置先が `osx-arm64` にハードコードされているのは Intel Mac 非サポートの
方針によるもので、意図的なもの。
```

- [ ] **Step 3: 記載どおりに再現できるか確認**

追記したコマンドをそのまま実行し、記載に誤りが無いことを確認する。

```bash
#!/bin/bash
set -u
. "$HOME/.cargo/env"
export CARGO_TARGET_DIR="$HOME/cef-target-mnt"
WORKTREE=/mnt/f/GitHub/cef-unity/.claude/worktrees/linux-phase2
cd "$WORKTREE/cef-unity-rust" || exit 1
bash deploy-linux.sh 2>&1 | tail -3
```

Expected: `[deploy-linux] done -> .../Plugins/linux-x64`

- [ ] **Step 4: コミット**

```bash
# git は Windows 側 (worktree ディレクトリ) から実行する
git add cef-unity-rust/CLAUDE.md
git commit -m "docs: CLAUDE.md に deploy-linux.sh の手順とサポート対象表の更新を反映"
git push
```

---

## 完了条件

1. `build-server-sandbox.sh` のリファクタで配置物が変わっていない (`libcef_unity_rust.so` の追加を除く)
2. `deploy-linux.sh` が `Plugins/linux-x64/` に成果物一式を配置し、`ldd` に `not found` が無い
3. `.gitattributes` に `linux-x64/**` の LFS フィルタがある
4. `generate-missing-meta.sh` が Linux の `.so` に `Linux64: enabled: 1` の meta を生成し、既存の Windows meta を壊していない
5. **CI の `build-linux (x64)` と `build-linux (arm64)` が双方とも緑で、`SMOKE_OK frames=N` (N > 0) が出る**
6. `Plugins/linux-x64/` のバイナリがこのブランチにコミットされていない

5 番が本フェーズの合否判定。
