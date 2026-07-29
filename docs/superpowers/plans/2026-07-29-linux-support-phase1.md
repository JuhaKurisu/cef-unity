# Linux サポート フェーズ 1 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Linux (x86_64) で `cef-unity-server` が起動し、software paint 経路で共有メモリに載った BGRA フレームを C# Harness から読み出して PNG に落とせる状態にする。

**Architecture:** Linux 固有のサブシステムは新設しない。既存の「非 macOS = software paint + generic event loop」経路を開通させるだけ。CEF は `cef-dll-sys` が自動取得する Linux 配布物をフラット配置し、`RPATH=$ORIGIN` で `libcef.so` を解決する。

**Tech Stack:** Rust 2024 edition / `cef` crate 145.5.0 / `shared_memory` / `ipc-channel` / .NET 10 (C# Harness) / WSL2 Ubuntu 24.04

設計書: `docs/superpowers/specs/2026-07-29-linux-support-phase1-design.md`

## Global Constraints

- **命名規約**: 識別子は省略形を使わずフルネーム (`buffer`, `index`, `width`, `height` 等)。ルート `CLAUDE.md` の規約に従う。シェル変数も同様
- **アーキテクチャ分岐を導入しない**: `#[cfg(target_arch)]` を追加しない。CEF 配布物の探索はワイルドカード `cef_linux_*` を使う (既存 `cef_macos_*` と同じ方式)
- **Intel Mac (x86_64 macOS) はサポートしない** (2026-07-29 決定)。`deploy.sh` の `osx-arm64` ハードコードは意図的なもので修正しない
- **既存の macOS / Windows 経路の挙動を変えない**: 追加する分岐はすべて `#[cfg(target_os = "linux")]` または MSBuild の `IsOSPlatform('Linux')` の内側に閉じる
- **文字列リテラルは挙動契約**: dev トグル、プロトコル文字列、CLI 引数、環境変数名を変更しない
- **作業場所**: WSL2 Ubuntu 24.04 の `~/cef-unity` (ext4 側)。`/mnt/f` 直参照は使わない
- **フェーズ 1 のスコープ外**: Unity 向け deploy (`Plugins/linux-x64/`)、GPU ゼロコピー (dmabuf/EGL)、ネイティブ音声出力、GitHub Actions の ubuntu ジョブ

## File Structure

| ファイル | 区分 | 責務 |
|---|---|---|
| `cef-unity-rust/crates/server/src/server.rs` | 変更 | `cef_window_handle_t` の Linux 分岐 (1174-1180 行) |
| `cef-unity-rust/crates/server/build.rs` | 変更 | Linux で `RPATH=$ORIGIN` を付与 |
| `cef-unity-rust/crates/helper/build.rs` | **新規** | 同上 (helper には現在 build.rs が無い) |
| `cef-unity-rust/build-server-sandbox.sh` | 変更 | OS 分岐。macOS = 既存の .app バンドル、Linux = フラット配置 |
| `cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj` | 変更 | パス区切りのスラッシュ化、`.so` / `.dylib` の OS 分岐、ステージング条件の Linux 追加 |
| `cef-unity-csharp/CefUnity.Harness/PortableNetworkGraphicsWriter.cs` | **新規** | BGRA バッファを PNG に書き出す最小エンコーダ (診断用) |
| `cef-unity-csharp/CefUnity.Harness/Program.cs` | 変更 | `dump` サブコマンドを追加 |
| `cef-unity-rust/CLAUDE.md` | 変更 | Linux セクション、apt パッケージ一覧、サポート方針 |

`PortableNetworkGraphicsWriter` を `CefUnity.Core` ではなく `CefUnity.Harness` に置くのは、これが診断専用でランタイムの一部ではないため。Core は Unity が参照する単一の真実源であり、診断コードを混ぜない。

## 前提: 実行環境

このプランは WSL2 Ubuntu 24.04 上で実行する。以下は導入済みであること (未導入なら Task 0 を先に実施):

- `build-essential pkg-config curl git cmake python3`
- Rust stable ツールチェーン
- リポジトリのクローンが `~/cef-unity` にある

---

### Task 0: 実行環境の前提を確認する

**Files:** なし (確認のみ)

**Interfaces:**
- Consumes: なし
- Produces: 以降のすべての Task が前提とするビルド環境

- [ ] **Step 1: 必要なコマンドが揃っているか確認**

```bash
cd ~/cef-unity/cef-unity-rust
. "$HOME/.cargo/env"
cargo --version && gcc --version | head -1 && readelf --version | head -1
```

Expected: 3 つともバージョンが表示される。`command not found` が出たら
`sudo apt-get install -y build-essential pkg-config curl git cmake python3` を実行する。

- [ ] **Step 2: CEF 実行時依存パッケージを導入**

```bash
sudo apt-get install -y \
  libnss3 libnspr4 libasound2t64 libatk1.0-0t64 libatk-bridge2.0-0t64 \
  libcups2t64 libdrm2 libgbm1 libgtk-3-0t64 libxcomposite1 libxdamage1 \
  libxfixes3 libxkbcommon0 libxrandr2 libpango-1.0-0 libcairo2 libx11-xcb1 libxss1
```

- [ ] **Step 3: ベースラインのテストが通ることを確認**

```bash
cd ~/cef-unity/cef-unity-rust && . "$HOME/.cargo/env" && cargo test -p cef-unity-ipc
```

Expected: `test result: ok. 19 passed; 0 failed`

コミットは不要 (ファイル変更なし)。

---

### Task 1: `cef_window_handle_t` の Linux 分岐

`cef-dll-sys` のターゲット別バインディングでは `cef_window_handle_t` が
macOS = `*mut c_void`、Linux = `c_ulong` (X11 Window / XID)、Windows = `HWND` と
3 者すべて異なる。現状のコードは `#[cfg(not(target_os = "windows"))]` で macOS と Linux を
まとめており、Linux でビルドが通らない。

**Files:**
- Modify: `cef-unity-rust/crates/server/src/server.rs:1174-1180`

**Interfaces:**
- Consumes: なし
- Produces: Linux でコンパイル可能な `cef-unity-server` バイナリ。Task 2 以降の全 Task が前提とする

- [ ] **Step 1: ビルドが失敗することを確認する (失敗するテスト)**

```bash
cd ~/cef-unity/cef-unity-rust && . "$HOME/.cargo/env" && cargo build 2>&1 | tail -20
```

Expected: FAIL。以下のエラーが出る。

```
error[E0308]: mismatched types
    --> crates/server/src/server.rs:1181:71
     |
1181 | ...ult().set_as_windowless(parent_handle);
     |          ----------------- ^^^^^^^^^^^^^ expected `u64`, found `*mut _`
```

- [ ] **Step 2: 3 分岐に割る**

`crates/server/src/server.rs` の 1174-1180 行を以下で置き換える。

置換前:

```rust
        // cef_window_handle_t はプラットフォーム依存:
        //   macOS / Linux: *mut c_void
        //   Windows: HWND (newtype wrapping *mut c_void)
        #[cfg(target_os = "windows")]
        let parent_handle = cef::sys::HWND(std::ptr::null_mut());
        #[cfg(not(target_os = "windows"))]
        let parent_handle = std::ptr::null_mut();
```

置換後:

```rust
        // cef_window_handle_t はプラットフォーム依存:
        //   macOS: *mut c_void
        //   Linux: c_ulong (X11 Window / XID)
        //   Windows: HWND (newtype wrapping *mut c_void)
        #[cfg(target_os = "windows")]
        let parent_handle = cef::sys::HWND(std::ptr::null_mut());
        #[cfg(target_os = "macos")]
        let parent_handle = std::ptr::null_mut();
        #[cfg(target_os = "linux")]
        let parent_handle: cef::sys::cef_window_handle_t = 0;
```

- [ ] **Step 3: ビルドが通ることを確認**

```bash
cd ~/cef-unity/cef-unity-rust && . "$HOME/.cargo/env" && cargo build 2>&1 | tail -5
```

Expected: PASS。`Finished \`dev\` profile` が出る。エラーなし (warning は既存のものが残る)。

- [ ] **Step 4: 既存テストが壊れていないことを確認**

```bash
cd ~/cef-unity/cef-unity-rust && . "$HOME/.cargo/env" && cargo test -p cef-unity-ipc 2>&1 | tail -5
```

Expected: `test result: ok. 19 passed; 0 failed`

- [ ] **Step 5: コミット**

```bash
cd ~/cef-unity
git add cef-unity-rust/crates/server/src/server.rs
git commit -m "fix(server): Linux の cef_window_handle_t は c_ulong なので分岐を 3 系統に割る"
```

---

### Task 2: `RPATH=$ORIGIN` の付与

`libcef.so` は実行ファイルと同じディレクトリに配置される。RPATH が無いと
`error while loading shared libraries: libcef.so` で起動できず、`LD_LIBRARY_PATH` の
設定を利用者に強いることになる。macOS は framework の動的ロード、Windows は
同一ディレクトリ探索で解決済みなので、Linux だけの追加となる。

**Files:**
- Modify: `cef-unity-rust/crates/server/build.rs`
- Create: `cef-unity-rust/crates/helper/build.rs`

**Interfaces:**
- Consumes: Task 1 の成果 (Linux でビルドが通ること)
- Produces: `RUNPATH=$ORIGIN` を持つ `cef-unity-server` と `cef-unity-rust-helper`。Task 3 のフラット配置がこれに依存する

- [ ] **Step 1: RUNPATH が無いことを確認する (失敗するテスト)**

```bash
cd ~/cef-unity/cef-unity-rust
readelf -d target/debug/cef-unity-server | grep -E 'RPATH|RUNPATH' || echo "NO_RUNPATH"
readelf -d target/debug/cef-unity-rust-helper | grep -E 'RPATH|RUNPATH' || echo "NO_RUNPATH"
```

Expected: FAIL。両方とも `NO_RUNPATH` が出る。

- [ ] **Step 2: `crates/server/build.rs` に RPATH 指定を追加**

`fn main() {` の直後 (既存の `#[cfg(target_os = "macos")]` ブロックより前) に以下を挿入する。

```rust
    // Linux: libcef.so は実行ファイルと同じディレクトリに配置されるため、
    // $ORIGIN を RPATH に入れて LD_LIBRARY_PATH なしで解決させる。
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
```

- [ ] **Step 3: `crates/helper/build.rs` を新規作成**

`cef-unity-rust/crates/helper/build.rs` を以下の内容で作成する。

```rust
fn main() {
    // Linux: libcef.so は実行ファイルと同じディレクトリに配置されるため、
    // $ORIGIN を RPATH に入れて LD_LIBRARY_PATH なしで解決させる。
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
}
```

- [ ] **Step 4: 再ビルドして RUNPATH が入ったことを確認**

```bash
cd ~/cef-unity/cef-unity-rust && . "$HOME/.cargo/env" && cargo build 2>&1 | tail -3
readelf -d target/debug/cef-unity-server | grep -E 'RPATH|RUNPATH'
readelf -d target/debug/cef-unity-rust-helper | grep -E 'RPATH|RUNPATH'
```

Expected: PASS。両方に `Library runpath: [$ORIGIN]` が出る。

- [ ] **Step 5: コミット**

```bash
cd ~/cef-unity
git add cef-unity-rust/crates/server/build.rs cef-unity-rust/crates/helper/build.rs
git commit -m "build(linux): server/helper に RPATH=\$ORIGIN を付与し libcef.so を隣から解決する"
```

---

### Task 3: `build-server-sandbox.sh` の OS 分岐と Linux フラット配置

現状このスクリプトは macOS 専用 (`.app` バンドル生成 + `codesign`)。Linux では
バンドル概念が無いため、実行ファイルと CEF ランタイムを出力先へフラットに配置する。
配置内容は Windows の `copy-windows-runtime.ps1` に対応する。

CEF Linux 配布物の実際の内容 (実測):

```
chrome-sandbox  chrome_100_percent.pak  chrome_200_percent.pak  icudtl.dat
libEGL.so  libGLESv2.so  libcef.so  libvk_swiftshader.so  libvulkan.so.1
resources.pak  v8_context_snapshot.bin  vk_swiftshader_icd.json  locales/
```

Windows にある `snapshot_blob.bin` は Linux 配布物に存在しない。
`chrome-sandbox` は `settings.no_sandbox = 1` (`server.rs:957`) のため配置しない。

**Files:**
- Modify: `cef-unity-rust/build-server-sandbox.sh`

**Interfaces:**
- Consumes: Task 2 の成果 (`RUNPATH=$ORIGIN` を持つバイナリ)
- Produces: `build-server-sandbox.sh <output_directory>` が Linux でフラット配置を行う。
  Task 4 の csproj がこれを呼ぶ

- [ ] **Step 1: Linux で現状のスクリプトが失敗することを確認する (失敗するテスト)**

```bash
cd ~/cef-unity/cef-unity-rust
rm -rf /tmp/stage-test && mkdir -p /tmp/stage-test
bash build-server-sandbox.sh /tmp/stage-test; echo "exit=$?"
```

Expected: FAIL。`ERROR: CEF build output not found. Run 'cargo build' first.` が出て
exit != 0 (16 行目の glob が `cef_macos_*` のため Linux では一致しない)。

- [ ] **Step 2: スクリプトを OS 分岐させる**

`cef-unity-rust/build-server-sandbox.sh` の 12 行目 (`SCRIPT_DIR=...` の行) の直後に、
Linux 分岐を挿入する。macOS 側の既存処理 (13 行目以降) は一切変更しない。

12 行目の直後に挿入する内容:

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

- [ ] **Step 3: 配置が成功し、共有ライブラリがすべて解決することを確認**

```bash
cd ~/cef-unity/cef-unity-rust
rm -rf /tmp/stage-test && mkdir -p /tmp/stage-test
bash build-server-sandbox.sh /tmp/stage-test && echo "--- ldd ---" && \
  ldd /tmp/stage-test/cef-unity-server | grep "not found" || echo "ALL_RESOLVED"
```

Expected: PASS。`server staged (flat) at /tmp/stage-test` に続いて `ALL_RESOLVED` が出る
(`not found` の行が 1 つも無い)。

- [ ] **Step 4: 必要なファイルが揃っていることを確認**

```bash
cd /tmp/stage-test
for required in cef-unity-server cef-unity-rust-helper libcef.so icudtl.dat \
                resources.pak v8_context_snapshot.bin; do
    test -e "$required" && echo "ok   $required" || echo "MISSING $required"
done
test -d locales && echo "ok   locales/ ($(ls locales | wc -l) files)" || echo "MISSING locales/"
```

Expected: PASS。すべて `ok` で、`locales/` が 220 ファイル。

- [ ] **Step 5: LD_LIBRARY_PATH なしでサーバーが起動することを確認 (Task 2 の RPATH 検証)**

```bash
cd /tmp/stage-test && ./cef-unity-server 2>&1 | tail -3
```

Expected: PASS。`--ipc-server argument required` で panic する。
`error while loading shared libraries` が出たら RPATH が効いていない。

- [ ] **Step 6: コミット**

```bash
cd ~/cef-unity
git add cef-unity-rust/build-server-sandbox.sh
git commit -m "build(linux): build-server-sandbox.sh に Linux のフラット配置分岐を追加"
```

---

### Task 4: `CefUnity.Harness.csproj` の Linux 対応

2 つの問題がある。

**(a) パス区切り (7-8 行目)** — `'$(MSBuildThisFileDirectory)..\..\cef-unity-rust'` と
バックスラッシュ区切りで書かれている。Linux の MSBuild はバックスラッシュをディレクトリ
区切りとして扱わないため `Path.GetFullPath` が誤った値を返す。スラッシュ区切りに直す
(Windows / macOS でも正しく動く)。

**(b) 共有ライブラリ名とステージング条件 (16-24 行目)** — `.dylib` 決め打ちで、
条件が `IsOSPlatform('OSX')` に限定されている。

**Files:**
- Modify: `cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj`

**Interfaces:**
- Consumes: Task 3 の成果 (`build-server-sandbox.sh` が Linux で動くこと)
- Produces: Linux で `dotnet build` すると出力ディレクトリに
  `libcef_unity_rust.so` + `cef-unity-server` + CEF ランタイム一式が揃う。Task 5 がこれを実行する

- [ ] **Step 1: .NET 10 SDK を導入 (未導入の場合)**

```bash
which dotnet && dotnet --version || {
  curl -sSL https://dot.net/v1/dotnet-install.sh | bash -s -- --channel 10.0
  echo 'export PATH="$HOME/.dotnet:$PATH"' >> ~/.bashrc
  export PATH="$HOME/.dotnet:$PATH"
  dotnet --version
}
```

Expected: `10.` で始まるバージョンが表示される。

- [ ] **Step 2: 現状ビルドすると成果物が配置されないことを確認する (失敗するテスト)**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
rm -rf cef-unity-csharp/CefUnity.Harness/bin
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -3
ls cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0/libcef_unity_rust.so 2>/dev/null \
  || echo "MISSING libcef_unity_rust.so"
```

Expected: FAIL。ビルド自体は成功するが `MISSING libcef_unity_rust.so` が出る
(条件が `IsOSPlatform('OSX')` のため何も配置されない)。

- [ ] **Step 3: csproj を書き換える**

`cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj` の 7-8 行目を置き換える。

置換前:

```xml
    <RustProjectDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust'))</RustProjectDir>
    <RustTargetDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)..\..\cef-unity-rust\target\debug'))</RustTargetDir>
```

置換後 (バックスラッシュをスラッシュに。Windows / macOS でも同じく解決される):

```xml
    <RustProjectDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)../../cef-unity-rust'))</RustProjectDir>
    <RustTargetDir>$([System.IO.Path]::GetFullPath('$(MSBuildThisFileDirectory)../../cef-unity-rust/target/debug'))</RustTargetDir>
```

続いて 13-24 行目 (`<ItemGroup>` から `</Target>` まで) を置き換える。

置換前:

```xml
  <ItemGroup>
    <!-- debug 成果物はローカル開発専用。非 macOS / Rust 未ビルド環境では存在しないため
         Viewer と同じ OS + Exists ガードが必須 (無いと Windows で MSB3030) -->
    <None Include="$(RustTargetDir)/libcef_unity_rust.dylib"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.dylib"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And Exists('$(RustTargetDir)/libcef_unity_rust.dylib')" />
  </ItemGroup>
  <!-- cef-unity-server.app bundle を出力先に用意(旧 Sandbox と同じ) -->
  <Target Name="CopyServerApp" AfterTargets="Build"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And Exists('$(RustTargetDir)/cef-unity-server')">
    <Exec Command="bash '$(RustProjectDir)/build-server-sandbox.sh' '$(OutputPath)'" />
  </Target>
```

置換後:

```xml
  <ItemGroup>
    <!-- debug 成果物はローカル開発専用。Rust 未ビルド環境では存在しないため
         Viewer と同じ OS + Exists ガードが必須 (無いと Windows で MSB3030) -->
    <None Include="$(RustTargetDir)/libcef_unity_rust.dylib"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.dylib"
          Condition="$([MSBuild]::IsOSPlatform('OSX')) And Exists('$(RustTargetDir)/libcef_unity_rust.dylib')" />
    <None Include="$(RustTargetDir)/libcef_unity_rust.so"
          CopyToOutputDirectory="PreserveNewest" Link="libcef_unity_rust.so"
          Condition="$([MSBuild]::IsOSPlatform('Linux')) And Exists('$(RustTargetDir)/libcef_unity_rust.so')" />
  </ItemGroup>
  <!-- サーバーと CEF ランタイムを出力先に用意する。
       macOS は cef-unity-server.app バンドル、Linux はフラット配置 (スクリプト側で分岐)。 -->
  <Target Name="CopyServerApp" AfterTargets="Build"
          Condition="($([MSBuild]::IsOSPlatform('OSX')) Or $([MSBuild]::IsOSPlatform('Linux'))) And Exists('$(RustTargetDir)/cef-unity-server')">
    <Exec Command="bash '$(RustProjectDir)/build-server-sandbox.sh' '$(OutputPath)'" />
  </Target>
```

- [ ] **Step 4: ビルドして成果物が揃うことを確認**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
rm -rf cef-unity-csharp/CefUnity.Harness/bin
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -3
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
for required in libcef_unity_rust.so cef-unity-server cef-unity-rust-helper libcef.so \
                icudtl.dat resources.pak; do
    test -e "$required" && echo "ok   $required" || echo "MISSING $required"
done
```

Expected: PASS。ビルド成功かつすべて `ok`。

- [ ] **Step 5: macOS / Windows の条件を壊していないことを確認**

`.dylib` の `None` 項目と `IsOSPlatform('OSX')` 条件がそのまま残っており、
`CopyServerApp` の条件に `Or IsOSPlatform('Linux')` が足されただけであることを
diff で目視確認する。

```bash
cd ~/cef-unity && git diff cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj
```

Expected: `.dylib` 行が削除されていない。`IsOSPlatform('Windows')` の条件を新設していない。

- [ ] **Step 6: コミット**

```bash
cd ~/cef-unity
git add cef-unity-csharp/CefUnity.Harness/CefUnity.Harness.csproj
git commit -m "build(linux): Harness csproj のパス区切りをスラッシュ化し .so 配置と Linux ステージングを追加"
```

---

### Task 5: PNG ダンプ機能の追加

設計書の検証手順は「取得した BGRA を PNG に書き出して目視確認する」を合否判定としているが、
リポジトリに PNG 書き出しは存在しない。最小の PNG エンコーダを Harness に追加する。

PNG は zlib 圧縮を要求するが、.NET の `System.IO.Compression.ZLibStream` で足りるため
外部パッケージは不要。フィルタは None (各行の先頭に 0 バイト) を使う。

`CefUnity.Core` ではなく `CefUnity.Harness` に置く。Core は Unity が参照する単一の真実源であり、
診断専用コードを混ぜない。

**Files:**
- Create: `cef-unity-csharp/CefUnity.Harness/PortableNetworkGraphicsWriter.cs`
- Create: `cef-unity-csharp/CefUnity.Tests/PortableNetworkGraphicsWriterTests.cs`
- Modify: `cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj`
- Modify: `cef-unity-csharp/CefUnity.Harness/Program.cs`

**Interfaces:**
- Consumes: Task 4 の成果 (Linux でビルド・実行できる Harness)
- Produces:
  - `static void PortableNetworkGraphicsWriter.WriteBgra(string path, byte[] bgra, int width, int height)`
    — BGRA8 バッファ (行あたり `width * 4` バイト、上から下) を RGB PNG として書き出す。
    バッファが `width * height * 4` 未満なら `ArgumentException` を投げる
  - Harness の `dump <output-path>` サブコマンド

テストは `ProjectReference` ではなく **リンク済み `Compile` 項目**で Harness のソースを取り込む。
`CefUnity.Harness.csproj` には `CopyServerApp` ターゲット (Task 4 で Linux にも拡張済み) があり、
`ProjectReference` にすると `dotnet test` のたびに `build-server-sandbox.sh` が走ってしまうため。

- [ ] **Step 1: テストプロジェクトから PNG ライターのソースを参照する**

`cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj` の
`<None Include="fixtures/**" ... />` を含む `<ItemGroup>` の直前に、以下の `ItemGroup` を挿入する。

```xml
  <!-- Harness の診断用 PNG ライターを単体テストする。ProjectReference にすると
       Harness の CopyServerApp ターゲットが dotnet test のたびに走るためリンク参照にする。 -->
  <ItemGroup>
    <Compile Include="..\CefUnity.Harness\PortableNetworkGraphicsWriter.cs"
             Link="PortableNetworkGraphicsWriter.cs" />
  </ItemGroup>
```

- [ ] **Step 2: 失敗するテストを書く**

`cef-unity-csharp/CefUnity.Tests/PortableNetworkGraphicsWriterTests.cs` を新規作成する。

`CefUnity.Tests` は `ImplicitUsings` を有効にしていないため、`System` / `System.IO` を
明示的に `using` する。namespace は既存テストに合わせてブロックスコープで書く。

```csharp
using System;
using System.Buffers.Binary;
using System.IO;
using System.IO.Compression;
using System.Text;
using CefUnity.Harness;
using NUnit.Framework;

namespace CefUnity.Tests
{
    [TestFixture]
    public class PortableNetworkGraphicsWriterTests
    {
        private static readonly byte[] PngSignature = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };

        private static string WriteTemporaryPng(byte[] bgra, int width, int height)
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            PortableNetworkGraphicsWriter.WriteBgra(path, bgra, width, height);
            return path;
        }

        [Test]
        public void WriteBgra_WritesSignatureAndHeaderDimensions()
        {
            const int width = 3;
            const int height = 2;
            var path = WriteTemporaryPng(new byte[width * height * 4], width, height);
            try
            {
                var bytes = File.ReadAllBytes(path);
                Assert.That(bytes[..8], Is.EqualTo(PngSignature), "PNG シグネチャが一致しない");

                // 8..12 = IHDR の長さ, 12..16 = "IHDR", 16..20 = width, 20..24 = height
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(8)), Is.EqualTo(13));
                Assert.That(Encoding.ASCII.GetString(bytes, 12, 4), Is.EqualTo("IHDR"));
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(16)), Is.EqualTo(width));
                Assert.That(BinaryPrimitives.ReadInt32BigEndian(bytes.AsSpan(20)), Is.EqualTo(height));
                Assert.That(bytes[24], Is.EqualTo(8), "bit depth は 8");
                Assert.That(bytes[25], Is.EqualTo(2), "color type は truecolor");
            }
            finally
            {
                File.Delete(path);
            }
        }

        [Test]
        public void WriteBgra_ConvertsBgraToRgbInIdat()
        {
            // 1x1 画素。BGRA で B=0x10, G=0x20, R=0x30, A=0xFF → PNG では R,G,B の順
            var bgra = new byte[] { 0x10, 0x20, 0x30, 0xFF };
            var path = WriteTemporaryPng(bgra, 1, 1);
            try
            {
                var raw = InflateFirstIdat(File.ReadAllBytes(path));
                // 1 行 = フィルタバイト(0) + RGB 3 バイト
                Assert.That(raw, Is.EqualTo(new byte[] { 0x00, 0x30, 0x20, 0x10 }));
            }
            finally
            {
                File.Delete(path);
            }
        }

        [Test]
        public void WriteBgra_ThrowsWhenBufferIsTooSmall()
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            Assert.Throws<ArgumentException>(
                () => PortableNetworkGraphicsWriter.WriteBgra(path, new byte[4], 2, 2));
        }

        [Test]
        public void WriteBgra_ThrowsWhenDimensionsAreNotPositive()
        {
            var path = Path.Combine(Path.GetTempPath(), $"png-writer-test-{Guid.NewGuid():N}.png");
            Assert.Throws<ArgumentException>(
                () => PortableNetworkGraphicsWriter.WriteBgra(path, new byte[4], 0, 1));
        }

        /// <summary>最初の IDAT チャンクを取り出して zlib 展開する。</summary>
        private static byte[] InflateFirstIdat(byte[] png)
        {
            var offset = 8;
            while (offset < png.Length)
            {
                var length = BinaryPrimitives.ReadInt32BigEndian(png.AsSpan(offset));
                var type = Encoding.ASCII.GetString(png, offset + 4, 4);
                if (type == "IDAT")
                {
                    using var compressed = new MemoryStream(png, offset + 8, length);
                    using var inflate = new ZLibStream(compressed, CompressionMode.Decompress);
                    using var output = new MemoryStream();
                    inflate.CopyTo(output);
                    return output.ToArray();
                }
                offset += 12 + length; // length(4) + type(4) + data + crc(4)
            }
            throw new InvalidOperationException("IDAT chunk not found");
        }
    }
}
```

- [ ] **Step 3: テストが失敗することを確認**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
dotnet test cef-unity-csharp/CefUnity.Tests --filter PortableNetworkGraphicsWriterTests 2>&1 | tail -20
```

Expected: FAIL。`PortableNetworkGraphicsWriter.cs` がまだ存在しないため、
`CS0246: The type or namespace name 'PortableNetworkGraphicsWriter' could not be found`
に類するコンパイルエラーになる。

- [ ] **Step 4: PNG ライターを実装**

`cef-unity-csharp/CefUnity.Harness/PortableNetworkGraphicsWriter.cs` を新規作成する。

このファイルは `CefUnity.Tests` にもリンク参照される。Tests 側は `ImplicitUsings` を
有効にしていないため、**`System` / `System.IO` を明示的に `using` すること**
(Harness 側の暗黙 using に頼るとテストプロジェクトでコンパイルできない)。

```csharp
using System;
using System.Buffers.Binary;
using System.IO;
using System.IO.Compression;

namespace CefUnity.Harness
{

/// <summary>
///     BGRA8 バッファを PNG として書き出す最小エンコーダ (診断専用)。
///     フィルタは None、色型は Truecolor (RGB 8bit) を使う。
/// </summary>
public static class PortableNetworkGraphicsWriter
{
    private static readonly byte[] Signature = { 0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A };

    public static void WriteBgra(string path, byte[] bgra, int width, int height)
    {
        if (width <= 0 || height <= 0)
            throw new ArgumentException($"invalid dimensions: {width}x{height}");
        var expected = (long)width * height * 4;
        if (bgra.Length < expected)
            throw new ArgumentException($"buffer too small: {bgra.Length} < {expected}");

        // フィルタバイト (0 = None) + RGB 3 バイト/画素 を行ごとに並べる
        var raw = new byte[(1 + (long)width * 3) * height];
        var rawIndex = 0;
        for (var y = 0; y < height; y++)
        {
            raw[rawIndex++] = 0;
            var rowStart = (long)y * width * 4;
            for (var x = 0; x < width; x++)
            {
                var pixel = rowStart + (long)x * 4;
                raw[rawIndex++] = bgra[pixel + 2]; // R
                raw[rawIndex++] = bgra[pixel + 1]; // G
                raw[rawIndex++] = bgra[pixel + 0]; // B
            }
        }

        using var compressed = new MemoryStream();
        using (var deflate = new ZLibStream(compressed, CompressionLevel.Optimal, leaveOpen: true))
        {
            deflate.Write(raw, 0, raw.Length);
        }

        using var file = File.Create(path);
        file.Write(Signature);

        var header = new byte[13];
        BinaryPrimitives.WriteInt32BigEndian(header.AsSpan(0), width);
        BinaryPrimitives.WriteInt32BigEndian(header.AsSpan(4), height);
        header[8] = 8;  // bit depth
        header[9] = 2;  // color type: truecolor
        header[10] = 0; // compression: deflate
        header[11] = 0; // filter: adaptive
        header[12] = 0; // interlace: none
        WriteChunk(file, "IHDR", header);
        WriteChunk(file, "IDAT", compressed.ToArray());
        WriteChunk(file, "IEND", Array.Empty<byte>());
    }

    private static void WriteChunk(Stream stream, string type, byte[] data)
    {
        var length = new byte[4];
        BinaryPrimitives.WriteInt32BigEndian(length, data.Length);
        stream.Write(length);

        var typeBytes = new byte[4];
        for (var index = 0; index < 4; index++) typeBytes[index] = (byte)type[index];
        stream.Write(typeBytes);
        stream.Write(data);

        var crc = ComputeCyclicRedundancyCheck(typeBytes, data);
        var crcBytes = new byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(crcBytes, crc);
        stream.Write(crcBytes);
    }

    private static readonly uint[] CrcTable = BuildCrcTable();

    private static uint[] BuildCrcTable()
    {
        var table = new uint[256];
        for (var index = 0u; index < 256; index++)
        {
            var value = index;
            for (var bit = 0; bit < 8; bit++)
                value = (value & 1) != 0 ? 0xEDB88320u ^ (value >> 1) : value >> 1;
            table[index] = value;
        }
        return table;
    }

    private static uint ComputeCyclicRedundancyCheck(byte[] type, byte[] data)
    {
        var crc = 0xFFFFFFFFu;
        foreach (var value in type) crc = CrcTable[(crc ^ value) & 0xFF] ^ (crc >> 8);
        foreach (var value in data) crc = CrcTable[(crc ^ value) & 0xFF] ^ (crc >> 8);
        return crc ^ 0xFFFFFFFFu;
    }
}

}
```

- [ ] **Step 5: テストが通ることを確認**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
dotnet test cef-unity-csharp/CefUnity.Tests --filter PortableNetworkGraphicsWriterTests 2>&1 | tail -10
```

Expected: PASS。4 件すべて成功する。

- [ ] **Step 6: 既存テストを壊していないことを確認**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
dotnet test cef-unity-csharp/CefUnity.Tests 2>&1 | tail -10
```

Expected: PASS。既存テストの失敗数が 0 のまま。

- [ ] **Step 7: `dump` サブコマンドを追加**

`cef-unity-csharp/CefUnity.Harness/Program.cs` の 3-4 行目のコメントと変数宣言を書き換え、
`replay` 分岐の前に `dump` 分岐を挿入する。

3 行目のコメントを置き換える。

置換前:

```csharp
// サブコマンド: (なし)=スモーク, replay=Phase 4 で追加
```

置換後:

```csharp
// サブコマンド: (なし)=スモーク, dump=1 フレームを PNG 保存, replay=Phase 4 で追加
```

27 行目 (`}` で smoke 分岐が閉じた直後、`if (command == "replay")` の前) に以下を挿入する。

```csharp
if (command == "dump")
{
    var outputPath = args.Length > 1 ? args[1] : "frame.png";
    CefRuntime.Initialize(useGpu: false);
    var written = false;
    using (var browser = new Browser(1280, 720, "https://example.com"))
    {
        for (var frameIndex = 0; frameIndex < 600 && !written; frameIndex++)
        {
            browser.SendExternalBeginFrame((ulong)frameIndex);
            CefRuntime.Pump();
            Thread.Sleep(16);
            // 最初の 120 フレームはページのロード待ちに使い、白紙を掴まないようにする
            if (frameIndex < 120) continue;
            if (browser.TryGetBuffer(out var bgra, out var width, out var height))
            {
                CefUnity.Harness.PortableNetworkGraphicsWriter.WriteBgra(outputPath, bgra, width, height);
                Console.WriteLine($"DUMP_OK {outputPath} {width}x{height}");
                written = true;
            }
        }
    }
    CefRuntime.Shutdown();
    if (!written) Console.Error.WriteLine("DUMP FAIL: no frame captured");
    return written ? 0 : 1;
}
```

- [ ] **Step 8: Harness がビルドできることを確認**

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -5
```

Expected: PASS。`Build succeeded`。

- [ ] **Step 9: コミット**

```bash
cd ~/cef-unity
git add cef-unity-csharp/CefUnity.Harness/PortableNetworkGraphicsWriter.cs \
        cef-unity-csharp/CefUnity.Harness/Program.cs \
        cef-unity-csharp/CefUnity.Tests/PortableNetworkGraphicsWriterTests.cs \
        cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj
git commit -m "feat(harness): BGRA を PNG に書き出す dump サブコマンドと単体テストを追加"
```

---

### Task 6: Linux でのスモーク実行 — フェーズ 1 の合否判定

ここがフェーズ 1 の本体。設計書の「未知のリスク」(ozone/X11、zygote、external BeginFrame) が
現れるとすればこの Task。ここまでの Task はすべてビルド時の話であり、CEF を実際に
初期化するのはこれが初めてとなる。

**Files:**
- Modify (必要になった場合のみ): `cef-unity-rust/crates/server/src/server.rs:777` 付近
  (command line switch の追加箇所)

**Interfaces:**
- Consumes: Task 1-5 のすべて
- Produces: Linux で software paint 経由のフレーム取得が成立するという実証

- [ ] **Step 1: スモークを実行する**

```bash
cd ~/cef-unity/cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
./CefUnity.Harness smoke 2>&1 | tail -20; echo "exit=$?"
```

Expected: `SMOKE_OK frames=N` (N > 0) が出て exit 0。

- [ ] **Step 2: 失敗した場合の診断**

Step 1 が失敗した場合、以下の順に切り分ける。**成功した場合はこのステップを飛ばす。**

```bash
# サーバー側のログ (server.rs の log_file 設定先)
ls -la /tmp/cef_unity_*.log 2>/dev/null && tail -50 /tmp/cef_unity_debug.log 2>/dev/null
tail -80 /tmp/cef_unity_server.log 2>/dev/null
# X11 が見えているか
echo "DISPLAY=$DISPLAY"; ls -la /tmp/.X11-unix/ 2>/dev/null
```

**前提として既に手当て済みのもの** (重複して追加しないこと):

- `disable-gpu` / `disable-gpu-compositing` は CPU モード (`use_gpu = false`) で既に付与される
  (`server.rs:779-784`)。Harness の smoke は `CefRuntime.Initialize(useGpu: false)` を使うため
  この経路に入る。GPU 起因の失敗が出ても、まずこれらが効いていることをログで確認する
- `disable-gpu-sandbox` は無条件で付与済み (`server.rs:777`)
- CEF レベルのサンドボックスは `settings.no_sandbox = 1` で無効 (`server.rs:957`)

想定される失敗と対処:

| 症状 | 原因 | 対処 |
|---|---|---|
| ログに `Unable to open X display` / `ozone` 関連のエラー | 表示バックエンドが見つからない | 下記 (A) の `ozone-platform=headless` を追加 |
| helper が即座に終了する / zygote 関連のログ | zygote プロセスの扱い | 下記 (B) の `no-zygote` を追加 |
| `SMOKE_OK frames=0` (初期化は成功するがフレームが来ない) | external BeginFrame が software paint を駆動していない | 下記 (C) の切り分けを行う |

**(A) ozone-platform を headless にする**

`server.rs:777` の `disable-gpu-sandbox` の行の直後に挿入する。

```rust
                // Linux: OSR でも Chromium は表示バックエンドを要求する。ヘッドレス環境や
                // X11 が見えない環境で初期化が失敗するため headless バックエンドを指定する。
                #[cfg(target_os = "linux")]
                command_line.append_switch_with_value(
                    Some(&CefString::from("ozone-platform")),
                    Some(&CefString::from("headless")),
                );
```

**(B) zygote を無効化する**

同じ場所に挿入する。(A) と併用してよい。

```rust
                // Linux: zygote プロセスの fork がヘルパー配置と噛み合わない場合に無効化する。
                #[cfg(target_os = "linux")]
                command_line.append_switch(Some(&CefString::from("no-zygote")));
```

**(C) external BeginFrame の切り分け**

`Program.cs` の smoke ループから `browser.SendExternalBeginFrame((ulong)frameIndex);` の行を
一時的にコメントアウトして実行する。

- フレームが来る → external BeginFrame が Linux の software paint を駆動できていない。
  **コードは元に戻し**、設計書「未知のリスク」に結果を追記してフェーズ 2 に送る
  (`window_info.external_begin_frame_enabled = 1` は `server.rs:1188` 付近で全プラットフォーム
  共通に立っており、ここを Linux だけ変えるのはフェーズ 1 のスコープを超える)
- フレームが来ない → BeginFrame とは無関係。`on_paint` 自体が呼ばれていないので
  サーバーログ (`/tmp/cef_unity_server.log`) の `on_paint` 関連行を確認する

スイッチを追加した場合は `cargo build` → Task 3 の staging → 再実行する。追加したスイッチは
**必ず `#[cfg(target_os = "linux")]` で囲い**、macOS / Windows の挙動を変えないこと。

- [ ] **Step 3: PNG ダンプを実行して目視確認する**

```bash
cd ~/cef-unity/cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
./CefUnity.Harness dump /tmp/linux-frame.png 2>&1 | tail -5
file /tmp/linux-frame.png
```

Expected: `DUMP_OK /tmp/linux-frame.png 1280x720` が出て、
`file` が `PNG image data, 1280 x 720, 8-bit/color RGB, non-interlaced` を返す。

`file` が PNG と認識しない場合は Task 5 のエンコーダにバグがある。

- [ ] **Step 4: 画像を目視確認する**

```bash
cp /tmp/linux-frame.png /mnt/f/GitHub/cef-unity/linux-frame.png
```

Windows 側から `F:\GitHub\cef-unity\linux-frame.png` を開き、example.com のページが
描画されていることを確認する。真っ白 / 真っ黒の場合はフレームは流れているが描画内容が
無いということなので、Step 2 の表を再度参照する。

確認後、リポジトリに残さないよう削除する。

```bash
rm -f /mnt/f/GitHub/cef-unity/linux-frame.png
```

- [ ] **Step 5: コミット (スイッチ追加が必要だった場合のみ)**

Step 2 で `server.rs` にスイッチを追加した場合のみコミットする。変更が無ければ飛ばす。

```bash
cd ~/cef-unity
git add cef-unity-rust/crates/server/src/server.rs
git commit -m "fix(linux): CEF 初期化に必要な command line switch を追加"
```

---

### Task 7: `cef-unity-rust/CLAUDE.md` に Linux セクションを追記

フェーズ 1 の成果と前提を、次に触る人が再現できる形で記録する。

**Files:**
- Modify: `cef-unity-rust/CLAUDE.md`

**Interfaces:**
- Consumes: Task 6 の結果 (実際に必要だった switch や手順が確定していること)
- Produces: なし (ドキュメント)

- [ ] **Step 1: `### 3. Unity プロジェクトへのデプロイ` セクションの Windows 小節の直後に Linux 小節を追加**

以下を挿入する。Task 6 で command line switch の追加が必要だった場合は、
「既知の制約」に実際の内容を追記すること。

````markdown
#### Linux (x86_64)

フェーズ 1 時点では **Unity への deploy スクリプトは無い**。Rust 単体と C# Harness までの対応。

必要な apt パッケージ:

```bash
# ビルド
sudo apt-get install -y build-essential pkg-config curl git cmake python3
# CEF 実行時
sudo apt-get install -y \
  libnss3 libnspr4 libasound2t64 libatk1.0-0t64 libatk-bridge2.0-0t64 \
  libcups2t64 libdrm2 libgbm1 libgtk-3-0t64 libxcomposite1 libxdamage1 \
  libxfixes3 libxkbcommon0 libxrandr2 libpango-1.0-0 libcairo2 libx11-xcb1 libxss1
```

Harness の実行:

```bash
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0
./CefUnity.Harness smoke          # フレームが取れるか
./CefUnity.Harness dump out.png   # 1 フレームを PNG 保存
```

`build-server-sandbox.sh` が Linux ではフラット配置を行う (macOS の .app バンドルに相当)。
`libcef.so` は `RPATH=$ORIGIN` で解決するため `LD_LIBRARY_PATH` の設定は不要。

既知の制約:

- **software paint 経路のみ**。GPU ゼロコピー (dmabuf/EGL) は未実装
- ネイティブ音声出力 (macOS の AudioUnit 経路に相当) は無い。Unity ミキサ経路のみ
- Unity Editor / Player 対応は未着手
````

- [ ] **Step 2: サポート対象アーキテクチャの方針を追記**

ファイル末尾の「**注意:**」の段落の直前に以下を挿入する。

```markdown
## サポート対象プラットフォーム

| プラットフォーム | 状態 |
|---|---|
| macOS arm64 | GPU ゼロコピー (IOSurface/Mach/Metal) |
| Windows x64 | GPU ゼロコピー (D3D11 共有テクスチャ + 共有 fence) |
| Linux x86_64 | software paint のみ。Rust + Harness まで (Unity 未対応) |
| macOS x86_64 (Intel Mac) | **サポートしない** |

`deploy.sh` の配置先が `osx-arm64` にハードコードされているのは Intel Mac 非サポートの
方針によるもので、意図的なもの。
```

- [ ] **Step 3: 記載どおりに再現できるか確認**

追記したコマンドをそのままコピーして実行し、記載に誤りが無いことを確認する。

```bash
cd ~/cef-unity && export PATH="$HOME/.dotnet:$PATH"
dotnet build cef-unity-csharp/CefUnity.Harness -c Debug 2>&1 | tail -3
cd cef-unity-csharp/CefUnity.Harness/bin/Debug/net10.0 && ./CefUnity.Harness smoke 2>&1 | tail -3
```

Expected: `SMOKE_OK frames=N`

- [ ] **Step 4: コミット**

```bash
cd ~/cef-unity
git add cef-unity-rust/CLAUDE.md
git commit -m "docs: CLAUDE.md に Linux セクションとサポート対象プラットフォームを追記"
```

---

## 完了条件

1. `cargo build` が Linux で成功する
2. `cargo test -p cef-unity-ipc` が 19/19 パスする
3. `readelf -d target/debug/cef-unity-server` に `RUNPATH: [$ORIGIN]` がある
4. `build-server-sandbox.sh <dir>` の出力に対し `ldd` が `not found` を返さない
5. `./CefUnity.Harness smoke` が `SMOKE_OK frames=N` (N > 0) を返す
6. `./CefUnity.Harness dump out.png` の出力を `file` が PNG と認識し、目視で example.com が描画されている
7. macOS / Windows のビルドが壊れていない (別マシンでの確認、またはフェーズ 1 の範囲外として次回に送る)

完了条件 7 について: この作業は WSL2 上でのみ行うため、macOS / Windows のビルドは
このフェーズ内では検証できない。すべての変更を `#[cfg(target_os = "linux")]` および
`IsOSPlatform('Linux')` の内側に閉じることで構造的に担保し、実際の確認は
Windows 側の作業ツリーに取り込んだ時点で `cargo build` と `dotnet build` を回して行う。
