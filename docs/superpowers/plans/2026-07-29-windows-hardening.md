# Windows 対応の底上げ 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Windows の残存リスク (D3D11 スレッド規約・プレイヤービルド未検証)、macOS との機能差 (ネイティブ音声・生スクロール入力・キーリピート)、品質面の小物 (ログ・seqlock) を解消し、ドキュメントを実装に追従させる。

**Architecture:** 既存のプラットフォーム分岐点を増やさない方針を守る。スクロールは `ScrollInputPipeline.StartNativeSource` の 1 箇所、音声は `cef_unity_audio_native_*` FFI の `#[cfg]` ブロック、テクスチャ同期は `d3d11.rs` / `d3d12.rs` に閉じ込める。Windows 固有の OS 呼び出しは Rust 側 (windows-rs) に置き、C# は P/Invoke の薄いラッパーに留める (macOS の `MacNativeScrollSource` と対称)。

**Tech Stack:** Rust (windows-rs, cef-rs), C# (netstandard2.1 / Unity 6000.3.8f1), Unity Native Plugin Interface, WASAPI, Win32 Raw Input

## Global Constraints

- 識別子は省略形を使わずフルネーム (`CLAUDE.md` の命名規約)。維持してよい頭字語は `id`/`url`/`ime`/`gpu`/`fps`/`ipc`/`ffi`/`osr`/`cef`/`d3d11`/`bgra`/`io`/`dsp`/`rms`
- 文字列リテラル (dev トグル名・プロトコル文字列・CSV フォーマット・CLI 引数・環境変数名) は挙動契約なので変更しない
- FFI 追加・変更時は `cef-unity-csharp/CefUnity.Core/Interop/` を単一の真実源として更新する (`NativeMethods.g.cs` は `crates/client/build.rs` の csbindgen が生成)
- Rust 変更後は必ず `deploy.ps1 -Arch x64` を実行する。C# 変更後は `bash cef-unity-csharp/build-csharp.sh`
- Windows のビルド/テストは MSVC 環境が必要。Git Bash から `cargo` を直接叩くと `/usr/bin/link` が MSVC の `link.exe` を隠して失敗するため、PowerShell から `vcvars64.bat` 経由で実行すること
- 作業は `.claude/worktrees/<name>` の git worktree に隔離する
- コミットに Claude の属性 (Co-Authored-By 等) を入れない

---

## Phase 1: 低リスクな品質修正

### Task 1: d3d11/d3d12 のログをマスターフラグに従わせる

**Files:**
- Create: `cef-unity-rust/crates/client/src/logging.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (`LOG_ENABLED` を `logging` へ移動、`log_to_file` を委譲)
- Modify: `cef-unity-rust/crates/client/src/d3d11.rs:28-37` (`log_debug`)
- Modify: `cef-unity-rust/crates/client/src/d3d12.rs:37-46` (`log_debug`)

**Interfaces:**
- Produces: `logging::set_enabled(bool)`, `logging::is_enabled() -> bool`, `logging::write(prefix: &str, message: &str)`

**問題:** `d3d11.rs` / `d3d12.rs` の `log_debug` は `LOG_ENABLED` を無視し、呼ばれるたびに `OpenOptions::open` する。fence 失敗が継続する異常系では毎フレーム file I/O = フレームスパイク源になる。

- [ ] **Step 1: 失敗するテストを書く**

`cef-unity-rust/crates/client/src/logging.rs` に配置する:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_is_suppressed_while_disabled() {
        set_enabled(false);
        assert!(!is_enabled(), "既定では無効であること");
        set_enabled(true);
        assert!(is_enabled(), "有効化が反映されること");
        set_enabled(false);
        assert!(!is_enabled(), "無効化が反映されること");
    }
}
```

- [ ] **Step 2: テストが失敗することを確認**

Run: `cmd /c '"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul && cargo test -p cef-unity-client'`
Expected: FAIL (`logging` モジュールが存在しない)

- [ ] **Step 3: 最小実装**

```rust
//! クライアント側ログの単一窓口。マスターフラグ (cef_unity_initialize の enable_log)
//! で全経路を制御し、ファイルハンドルを保持して毎回の open を避ける。

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};

static LOG_ENABLED: AtomicBool = AtomicBool::new(false);
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn set_enabled(enabled: bool) {
    LOG_ENABLED.store(enabled, Ordering::SeqCst);
    if !enabled {
        *LOG_FILE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

pub fn is_enabled() -> bool {
    LOG_ENABLED.load(Ordering::Relaxed)
}

/// `prefix` は経路の識別子 ("d3d11" / "d3d12" / "" など)。無効時は即 return。
pub fn write(prefix: &str, message: &str) {
    if !is_enabled() {
        return;
    }
    let mut guard = LOG_FILE.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        let path = std::env::temp_dir().join("cef_unity_debug.log");
        *guard = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
    }
    if let Some(file) = guard.as_mut() {
        if prefix.is_empty() {
            let _ = writeln!(file, "[{:?}] {}", std::time::SystemTime::now(), message);
        } else {
            let _ = writeln!(file, "[{}] {}", prefix, message);
        }
    }
}
```

`lib.rs` の `log_to_file` を `logging::write("", message)` へ委譲し、`LOG_ENABLED.store(...)` を `logging::set_enabled(...)` に置き換える。`d3d11.rs` / `d3d12.rs` の `log_debug` を `crate::logging::write("d3d11", message)` / `("d3d12", message)` に置き換える。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p cef-unity-client` (vcvars64 経由)
Expected: PASS

- [ ] **Step 5: 既存の全テストが通ることを確認**

Run: `cargo test` (vcvars64 経由)
Expected: 全パス

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/client/src/logging.rs cef-unity-rust/crates/client/src/lib.rs cef-unity-rust/crates/client/src/d3d11.rs cef-unity-rust/crates/client/src/d3d12.rs
git commit -m "fix(win): d3d11/d3d12 のログをマスターフラグに従わせハンドルを保持する"
```

---

### Task 2: software 経路の read_frame に seqlock を入れる

**Files:**
- Modify: `cef-unity-rust/crates/ipc/src/lib.rs` (`read_frame`)
- Test: `cef-unity-rust/crates/ipc/src/lib.rs` (`mod tests`)

**問題:** reader がコピーしている最中に writer が 2 フレーム進むと、コピー結果に新旧フレームが混在する (IPC-7)。GPU 経路が動く今も、software フォールバック時のリスクとして残る。

**設計:** コピー前後で `(frame_id, active_buffer)` を照合し、変化していたら **1 回だけ** リトライする。ヘッダ変更も writer 変更も不要。リトライは有限 (1 回) 固定 — 無限リトライは高負荷時に reader がスピンして 60fps を壊す。

- [ ] **Step 1: 失敗するテストを書く**

```rust
    /// コピー中に writer が進んだ場合、読み出し結果が新旧混在にならないこと。
    /// writer をコピーの「途中」で走らせる代わりに、コピー後に frame_id だけ
    /// 進める writer を模し、reader が再取得して最新フレームを返すことを見る。
    #[test]
    fn read_frame_retries_when_writer_advances_during_copy() {
        let flink = std::env::temp_dir()
            .join("cef-unity-test-shm-seqlock")
            .to_str()
            .unwrap()
            .to_string();

        let writer = SharedMemoryWriter::new(&flink).expect("SharedMemoryWriter::new");
        let mut reader = SharedMemoryReader::open(&flink).expect("SharedMemoryReader::open");

        let first: Vec<u8> = vec![0xAA; 16];
        writer.write_frame(&first, 2, 2);

        let mut buffer = Vec::new();
        assert_eq!(reader.read_frame(&mut buffer), Some((2, 2)));
        assert!(buffer.iter().all(|&b| b == 0xAA), "単一フレームの内容が揃うこと");

        // writer が 2 フレーム進んだ後の読み出しは、最新フレームで揃っていること
        let second: Vec<u8> = vec![0xBB; 16];
        let third: Vec<u8> = vec![0xCC; 16];
        writer.write_frame(&second, 2, 2);
        writer.write_frame(&third, 2, 2);
        assert_eq!(reader.read_frame(&mut buffer), Some((2, 2)));
        assert!(
            buffer.iter().all(|&b| b == 0xCC),
            "最新フレームで揃うこと (新旧混在しないこと): {:?}",
            &buffer[..4]
        );
    }
```

- [ ] **Step 2: テストが失敗する (または既存実装で偶然通る) ことを確認**

Run: `cargo test -p cef-unity-ipc read_frame_retries` (vcvars64 経由)
Expected: このテストは既存実装でも通り得る。**通ってしまう場合は、コピー後の再検証が無いことを示す形にテストを書き直すこと** — `read_frame` を分解し、コピー直後に `frame_id` を読み直して不一致ならリトライする内部関数 `read_frame_once` を切り出し、その戻り値 (`Option<(u32,u32)>` と「再試行が必要か」) を直接検証する

- [ ] **Step 3: 最小実装**

`read_frame` のコピー処理を以下の形に置き換える:

```rust
    pub fn read_frame(&mut self, destination: &mut Vec<u8>) -> Option<(u32, u32)> {
        let header = unsafe { &*(self.shared_memory.as_ptr() as *const SharedMemoryHeader) };
        let frame_id = header.frame_id.load(Ordering::Acquire);
        if frame_id == self.last_frame_id {
            return None;
        }

        // コピー中に writer が進むと新旧フレームが混在する。コピー前後で
        // (frame_id, active_buffer) を照合し、変化していたら 1 回だけ読み直す。
        // リトライを 1 回に固定するのは、高負荷時に reader がスピンして
        // フレームレートを壊すのを避けるため。
        for attempt in 0..2 {
            let observed_frame_id = header.frame_id.load(Ordering::Acquire);
            let active = header.active_buffer.load(Ordering::Acquire);
            let width = header.width.load(Ordering::Acquire);
            let height = header.height.load(Ordering::Acquire);
            if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
                return None;
            }
            let size = (width as usize) * (height as usize) * 4;
            let offset = SHARED_MEMORY_HEADER_SIZE + active as usize * BUFFER_SIZE;
            destination.clear();
            destination.extend_from_slice(unsafe {
                std::slice::from_raw_parts(self.shared_memory.as_ptr().add(offset), size)
            });

            let still_valid = header.frame_id.load(Ordering::Acquire) == observed_frame_id
                && header.active_buffer.load(Ordering::Acquire) == active;
            if still_valid || attempt == 1 {
                self.last_frame_id = observed_frame_id;
                return Some((width, height));
            }
        }
        None
    }
```

既存の `read_frame` 本体 (frame_id 更新位置・境界検証) と挙動が変わらないことを確認しながら置き換えること。特に `self.last_frame_id` は**コピーが確定してから**更新する。

- [ ] **Step 4: テストが通ることを確認**

Run: `cargo test -p cef-unity-ipc` (vcvars64 経由)
Expected: 全パス (既存の `shared_memory_write_read_roundtrip` を含む)

- [ ] **Step 5: コミット**

```bash
git add cef-unity-rust/crates/ipc/src/lib.rs
git commit -m "fix(ipc): software 経路の read_frame に seqlock を入れ新旧混在を防ぐ"
```

---

### Task 3: Windows のキーリピート値を OS 設定から取得する

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefKeyboardMapper.cs:110-119` (非 macOS のフォールバック)

**問題:** Windows は `0.5s` / `0.035s` の決め打ちで、OS のキーボード設定を反映しない。

**設計:** `SystemParametersInfo` の `SPI_GETKEYBOARDDELAY` (0x0016) / `SPI_GETKEYBOARDSPEED` (0x000A) を読む。
- delay: 0〜3 の段階値 → `(value + 1) * 250ms`
- speed: 0〜31 の段階値 → 約 2.5〜30 回/秒の線形補間 → 間隔は `1 / (2.5 + value * (30 - 2.5) / 31)` 秒

- [ ] **Step 1: 実装**

`#else` ブロックを以下に置き換える (macOS 側は変更しない):

```csharp
#elif UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN
        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        private static extern bool SystemParametersInfo(
            uint action, uint parameter, out uint value, uint update);

        private const uint SpiGetKeyboardDelay = 0x0016;
        private const uint SpiGetKeyboardSpeed = 0x000A;

        /// <summary>SPI_GETKEYBOARDDELAY は 0〜3 の段階値で、(値 + 1) × 250ms を表す。</summary>
        private static float GetOSKeyRepeatDelay()
        {
            try
            {
                if (SystemParametersInfo(SpiGetKeyboardDelay, 0, out var value, 0) && value <= 3)
                    return (value + 1) * 0.25f;
            }
            catch
            {
                // P/Invoke 不能環境 (将来のプラットフォーム変更等) では既定値へ
            }
            return 0.5f;
        }

        /// <summary>SPI_GETKEYBOARDSPEED は 0〜31 の段階値で、約 2.5〜30 回/秒に線形対応する。</summary>
        private static float GetOSKeyRepeatRate()
        {
            try
            {
                if (SystemParametersInfo(SpiGetKeyboardSpeed, 0, out var value, 0) && value <= 31)
                {
                    var repeatsPerSecond = 2.5f + value * ((30f - 2.5f) / 31f);
                    return 1f / repeatsPerSecond;
                }
            }
            catch
            {
            }
            return 0.035f;
        }
#else
```

ファイル冒頭の `#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX` で囲まれた `using System;` / `using System.Runtime.InteropServices;` を、Windows でも必要になるので `#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX || UNITY_STANDALONE_WIN || UNITY_EDITOR_WIN` に広げること。

- [ ] **Step 2: Unity でコンパイルを確認**

Run: `uloop compile --project-path <worktree>/cef-unity-unityproject`
Expected: `ErrorCount: 0`

- [ ] **Step 3: 実機で値を確認**

Play モードに入り、Console に出る初期化ログとキー長押しの挙動で、リピートが OS 設定に追従することを確認する。

- [ ] **Step 4: コミット**

```bash
git add cef-unity-unityproject/Assets/CefUnity/Runtime/CefKeyboardMapper.cs
git commit -m "feat(win): キーリピート値を OS 設定 (SystemParametersInfo) から取得する"
```

---

## Phase 2: D3D11 immediate context のスレッド規約

### Task 4: 呼び出しスレッドの記録とアサートを入れる (段階 1)

**Files:**
- Modify: `cef-unity-rust/crates/client/src/d3d11.rs` (`wait_fence` 周辺 + モジュール doc)
- Modify: `cef-unity-rust/CLAUDE.md` (スレッド規約の明記)

**問題:** `d3d11.rs:216-253` は Unity の immediate context を取得し `ID3D11DeviceContext4::Wait` を発行するが、これは C# の PostLateUpdate = メインスレッドから呼ばれる。D3D11 の immediate context は非スレッドセーフ COM で Unity は render thread から使うため、競合すると DEVICE_REMOVED 系の障害になり得る。D3D12 の `ID3D12CommandQueue::Wait` はスレッドセーフなので D3D12 経路は無害。

**設計:** いきなり render thread へ移すのは C# のフロー変更 + fence 実証の再検証が必要で risk が高い。まず「どのスレッドから呼ばれているか」を実測できるようにする。

- [ ] **Step 1: 実装**

`d3d11.rs` に呼び出しスレッド ID を記録し、初回と変化時にログする:

```rust
/// `wait_fence` を最初に呼んだスレッド ID。immediate context は非スレッドセーフなので、
/// 複数スレッドから呼ばれていないことを実測で確認するために記録する。
static WAIT_FENCE_THREAD_ID: AtomicU64 = AtomicU64::new(0);

fn record_wait_fence_thread() {
    // ThreadId は不透明なので OS のスレッド ID を使う
    let current = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() } as u64;
    let previous = WAIT_FENCE_THREAD_ID.swap(current, Ordering::Relaxed);
    if previous != 0 && previous != current {
        crate::logging::write(
            "d3d11",
            &format!(
                "WARNING: wait_fence called from a different thread ({} -> {}). \
                 ID3D11DeviceContext は非スレッドセーフのため競合の可能性がある",
                previous, current
            ),
        );
    }
}
```

`wait_fence` の先頭で `record_wait_fence_thread()` を呼ぶ。モジュール冒頭の doc コメントに「`wait_fence` / `open_or_cached` は Unity のメインスレッドからのみ呼ぶこと。immediate context は非スレッドセーフで、render thread と競合し得る」と明記する。

- [ ] **Step 2: ビルドとデプロイ**

Run: `cargo build --release` → `deploy.ps1 -Arch x64` (vcvars64 経由)

- [ ] **Step 3: 実機で計測**

ProjectSettings の Windows graphics API を D3D11 優先 (`m_APIs: 0200000012000000`, `m_Automatic: 0`) に変更して Unity を再起動し、Play で 1 分程度動かす。`%TEMP%\cef_unity_debug.log` に WARNING が出ないことを確認する。確認後に設定を元 (`1200000002000000`, `m_Automatic: 1`) へ戻す。

- [ ] **Step 4: 結果を記録してコミット**

計測結果 (単一スレッドだったか) を `docs/REFACTORING_REPORT.md` の CLI-12 に追記する。

```bash
git add cef-unity-rust/crates/client/src/d3d11.rs cef-unity-rust/CLAUDE.md docs/REFACTORING_REPORT.md
git commit -m "feat(win): d3d11 の呼び出しスレッドを記録しスレッド規約を明記する"
```

**注:** 段階 2 (`GetRenderEventFunc` で render thread に閉じ込める) は、段階 1 の計測で実際に複数スレッドから呼ばれていた場合のみ着手する。単一スレッドなら規約の明記とアサートで十分であり、C# フロー変更のリスクを負う理由がない。この判断はユーザーに報告してから行うこと。

---

## Phase 3: Windows プレイヤービルドの検証

### Task 5: Windows のクイックビルドを追加して実際に配布物を作る

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Editor/CefQuickBuild.cs` (`BuildWindows` を追加)

**問題:** `CefQuickBuild` は `BuildMac` しか無く、`PostProcessWindows` が実際に動いた形跡がない。Editor では動いても配布物で動かない可能性が残る。

**Interfaces:**
- Produces: `CefUnity.Editor.CefQuickBuild.BuildWindows` — `-executeMethod` から参照される完全修飾名になるため、以後改名しないこと

- [ ] **Step 1: 実装**

`BuildMac` と同じ構造で追加する (既存の `BuildMac` を読み、`target` と出力パスだけ差し替える):

```csharp
        [MenuItem("CefUnity/Build Windows Player (measure)")]
        public static void BuildWindows()
        {
            // BuildMac と同じ計測用設定。出力先だけ Windows 向けに差し替える。
            var options = new BuildPlayerOptions
            {
                scenes = new[] { "Assets/Scenes/SampleScene.unity" },
                locationPathName = "Build/Windows/CefUnitySample.exe",
                target = BuildTarget.StandaloneWindows64,
                options = BuildOptions.Development
            };
            var report = BuildPipeline.BuildPlayer(options);
            Debug.Log("[CefQuickBuild] Windows build result: " + report.summary.result);
        }
```

実際の `BuildMac` の実装に合わせること (フィールド名・オプション・ログ形式)。

- [ ] **Step 2: ビルドを実行**

Run: `uloop execute-menu-item` は security 設定で無効なので、コマンドラインから実行する:

```powershell
& "F:\UnityEditor\6000.3.8f1\Editor\Unity.exe" -quit -batchmode `
  -projectPath <worktree>\cef-unity-unityproject `
  -executeMethod CefUnity.Editor.CefQuickBuild.BuildWindows `
  -logFile <worktree>\build-windows.log
```

Expected: 終了コード 0、`Build/Windows/CefUnitySample.exe` が生成される

- [ ] **Step 3: 配置物を検証**

`Build/Windows/CefUnitySample_Data/Plugins/x86_64/` に以下が揃っていることを確認する:
`cef_unity_rust.dll`, `cef-unity-server.exe`, `cef-unity-rust-helper.exe`, `libcef.dll`, `chrome_elf.dll`, `icudtl.dat`, `resources.pak`, `chrome_100_percent.pak`, `chrome_200_percent.pak`, `v8_context_snapshot.bin`, `locales/` (中に `.pak` 群)

`.meta` ファイルが混入していないことも確認する。

- [ ] **Step 4: 起動して実際に描画されることを確認**

`CefUnitySample.exe` を起動し、ブラウザが描画されることをスクリーンショットで確認する。`%TEMP%\cef_unity_debug.log` にサーバー起動の記録が出ることも確認する。

- [ ] **Step 5: 見つかった不具合を修正**

`PostProcessWindows` の配置漏れ・パス誤りがあればここで直す。`Build/` は `.gitignore` に入れる (既に入っていなければ追加)。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-unityproject/Assets/CefUnity/Editor/CefQuickBuild.cs .gitignore
git commit -m "feat(win): Windows プレイヤーのクイックビルドを追加し配布物を検証する"
```

---

## Phase 4: Windows の生スクロール入力

### Task 6: Rust 側に Raw Input のスクロールモニタを実装する

**Files:**
- Create: `cef-unity-rust/crates/client/src/scroll_monitor_windows.rs`
- Modify: `cef-unity-rust/crates/client/src/scroll_monitor.rs` (プラットフォーム分岐)
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (FFI の `#[cfg]` を Windows へ拡張)

**問題:** 生スクロール入力 (C 案) は macOS の NSEvent 専用で、Windows は frame-polled fallback のまま。Unity の `Input.mouseScrollDelta` はフレーム境界で量子化されるため、リサンプラの恩恵を受けられない。

**設計:** macOS 版 (`scroll_monitor.m` 80 行 + `scroll_monitor.rs` 21 行) と対称にする。Windows は `RegisterRawInputDevices` で `HID_USAGE_GENERIC_MOUSE` を購読し、専用スレッドのメッセージループで `WM_INPUT` を受けて `RI_MOUSE_WHEEL` のホイールデルタをタイムスタンプ付きでリングバッファに積む。C# は既存の `IScrollEventSource` 経由で drain する。

**単位規約 (重要):** Windows の `WM_MOUSEWHEEL` は 120 = 1 ノッチ。既存の `ScrollInputEvent` は macOS 由来で「precise = CSS px / 非 precise = ノッチ数」の規約なので、**Windows は非 precise として `delta / 120.0` をノッチ数で渡す**。`WheelPixelsPerStep` の乗算は呼び出し側 (`ScrollResampler`) が行うため Rust 側では掛けないこと。高精度ホイール (120 未満の刻み) もそのまま比率で渡る。

**Interfaces:**
- Produces (FFI, csbindgen 経由で C# へ):
  - `cef_unity_scroll_monitor_start() -> i32` (0 = 成功、負値 = 失敗)
  - `cef_unity_scroll_monitor_stop()`
  - `cef_unity_scroll_monitor_drain(out_events: *mut ScrollMonitorEvent, capacity: i32) -> i32` (書き込んだ件数)
  - `#[repr(C)] struct ScrollMonitorEvent { timestamp_seconds: f64, delta_x: f32, delta_y: f32, is_precise: i32, phase: i32 }`

既存の macOS 版の FFI シグネチャを先に読み、**同一のシグネチャに合わせること** (C# 側を分岐させないため)。macOS 版と異なる形にしてはならない。

- [ ] **Step 1: macOS 版の FFI シグネチャを読む**

`cef-unity-rust/crates/client/src/scroll_monitor.rs` と `cef-unity-csharp/CefUnity.Core/ScrollInput/MacNativeScrollSource.cs` を読み、関数名・構造体レイアウト・戻り値の規約を書き出す。以降の実装はこれに合わせる。

- [ ] **Step 2: 失敗するテストを書く**

Raw Input は実 OS メッセージループが必要でユニットテストに向かないため、テストはリングバッファのロジックに限定する:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_drains_in_order_and_clears() {
        let buffer = ScrollEventBuffer::new();
        buffer.push(ScrollMonitorEvent { timestamp_seconds: 1.0, delta_x: 0.0, delta_y: 1.0, is_precise: 0, phase: 0 });
        buffer.push(ScrollMonitorEvent { timestamp_seconds: 2.0, delta_x: 0.0, delta_y: -2.0, is_precise: 0, phase: 0 });

        let mut destination = vec![ScrollMonitorEvent::default(); 8];
        let count = buffer.drain_into(&mut destination);

        assert_eq!(count, 2, "積んだ件数が drain されること");
        assert_eq!(destination[0].timestamp_seconds, 1.0, "FIFO 順であること");
        assert_eq!(destination[1].delta_y, -2.0);
        assert_eq!(buffer.drain_into(&mut destination), 0, "drain 後は空になること");
    }

    #[test]
    fn ring_buffer_drops_oldest_when_capacity_exceeded() {
        let buffer = ScrollEventBuffer::new();
        for index in 0..(SCROLL_EVENT_CAPACITY + 2) {
            buffer.push(ScrollMonitorEvent { timestamp_seconds: index as f64, delta_x: 0.0, delta_y: 1.0, is_precise: 0, phase: 0 });
        }
        let mut destination = vec![ScrollMonitorEvent::default(); SCROLL_EVENT_CAPACITY];
        let count = buffer.drain_into(&mut destination);
        assert_eq!(count, SCROLL_EVENT_CAPACITY, "容量を超えない");
        assert_eq!(destination[0].timestamp_seconds, 2.0, "最古が捨てられること");
    }
}
```

- [ ] **Step 3: テストが失敗することを確認**

Run: `cargo test -p cef-unity-client ring_buffer` (vcvars64 経由)
Expected: FAIL (`ScrollEventBuffer` が存在しない)

- [ ] **Step 4: リングバッファを実装してテストを通す**

`Mutex<VecDeque<ScrollMonitorEvent>>` と `SCROLL_EVENT_CAPACITY: usize = 256` で実装する。`push` は容量超過時に先頭を捨てる。`drain_into` は `destination.len()` と保持件数の小さい方まで書き込み、書いた分を取り除く。

- [ ] **Step 5: Raw Input スレッドを実装**

専用スレッドを立て、メッセージ専用ウィンドウ (`HWND_MESSAGE` の子) を作り、`RegisterRawInputDevices` で `usUsagePage = 0x01` / `usUsage = 0x02` (generic mouse) を `RIDEV_INPUTSINK` 付きで登録する。`WM_INPUT` で `GetRawInputData` を呼び、`RAWMOUSE.usButtonFlags & RI_MOUSE_WHEEL` のとき `usButtonData` を `i16` として取り出し、`delta_y = value as f32 / 120.0` を push する。タイムスタンプは `std::time::Instant` 起点の経過秒。停止は `PostThreadMessage(WM_QUIT)` で行い、スレッドを join する。

- [ ] **Step 6: FFI を追加して csbindgen を再生成**

`lib.rs` の macOS 限定 `#[cfg]` を Windows でも有効にし、Windows では `scroll_monitor_windows` を呼ぶ。`cargo build` で `NativeMethods.g.cs` が再生成されることを確認する。

- [ ] **Step 7: 全テストとビルドを確認**

Run: `cargo test` および `cargo build --release` (vcvars64 経由)
Expected: 全パス

- [ ] **Step 8: コミット**

```bash
git add cef-unity-rust/crates/client/src/ cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs
git commit -m "feat(win): Raw Input による生スクロールモニタを追加する"
```

---

### Task 7: C# 側に WindowsNativeScrollSource を追加して配線する

**Files:**
- Create: `cef-unity-csharp/CefUnity.Core/ScrollInput/WindowsNativeScrollSource.cs`
- Modify: `cef-unity-csharp/CefUnity.Core/ScrollInput/ScrollInputPipeline.cs:72-96` (`StartNativeSource` の分岐)
- Test: `cef-unity-csharp/CefUnity.Tests/` (既存のスクロールテストに倣う)

**設計:** `MacNativeScrollSource.cs` (57 行) と対称の薄い P/Invoke ラッパー。`StartNativeSource` の分岐に Windows 節を足す — 呼び出し側 (`CefUnityBrowserSample.SetupScrollInput`) には一切 `#if` を増やさない。

- [ ] **Step 1: MacNativeScrollSource を読んで対称に実装**

`IScrollEventSource` の実装 (`Start()` / `Dispose()` / イベント drain) を macOS 版と同じ形で書く。FFI 名は Task 6 で macOS と揃えてあるので、実質プラットフォーム判定だけが違う。

- [ ] **Step 2: `StartNativeSource` に Windows 節を追加**

```csharp
            if (System.Runtime.InteropServices.RuntimeInformation.IsOSPlatform(System.Runtime.InteropServices.OSPlatform.Windows))
            {
                var source = new WindowsNativeScrollSource();
                try
                {
                    if (source.Start())
                    {
                        _source = source;
                        return NativeScrollSourceStart.Started;
                    }
                }
                catch (Exception exception)
                {
                    error = exception;
                    source.Dispose();
                    return NativeScrollSourceStart.Failed;
                }
                source.Dispose();
                return NativeScrollSourceStart.Unavailable;
            }
```

- [ ] **Step 3: `SetupScrollInput` のログ文言を見直す**

`CefUnityBrowserSample.SetupScrollInput` の `Started` 分岐は `"scroll: native NSEvent source active"` と macOS 固有の文言になっている。プラットフォーム非依存の文言 (例: `"scroll: native source active"`) に直す。**ただしこれはログ文言であり dev トグル等の挙動契約ではないので変更してよい。**

- [ ] **Step 4: テストとビルド**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests/CefUnity.Tests.csproj -c Release`
Expected: 全パス

- [ ] **Step 5: 実機で確認**

`bash cef-unity-csharp/build-csharp.sh` の後、Unity の Play で Console に `scroll: native source active` が出ること、実際にホイールでスクロールできることを確認する。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-csharp/CefUnity.Core/ScrollInput/ cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs cef-unity-unityproject/Assets/CefUnity/Plugins/CefUnity.Core.dll
git commit -m "feat(win): 生スクロール入力を Windows で有効化する"
```

---

## Phase 5: Windows のネイティブ音声出力

### Task 8: WASAPI 出力を実装する

**Files:**
- Create: `cef-unity-rust/crates/client/src/wasapi_output.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs` (`cef_unity_audio_native_*` の `#[cfg]` を Windows へ拡張、`ClientBrowserInstance.native_voice` のフィールド gate)

**問題:** `cef_unity_audio_native_start` は非 macOS で `-1` を返し、Windows は常に Unity ミキサ経由。macOS の AudioUnit 直接出力による低遅延の恩恵が無い。

**設計:** `native_voice.rs` (325 行) が macOS で行っていることの Windows 版。WASAPI の共有モード・イベント駆動で `IAudioClient` / `IAudioRenderClient` を開き、専用スレッドが音声リングバッファ (`AudioSharedMemoryReader`) から読んで書き込む。リングの読み出しロジックは既存の `audio_ring.rs` を再利用し、**出力デバイス層だけを差し替える**。

**Interfaces:**
- Produces: `WasapiOutput::start(audio_flink: &str, target_milliseconds: f32, io_frames: i32) -> Result<WasapiOutput, String>`, `WasapiOutput::stop(self)`, `WasapiOutput::set_volume(f32)`, `WasapiOutput::statistics() -> (u64, u64, f32)`

`native_voice.rs` の `NativeVoice` と同じメソッド構成に揃え、`lib.rs` の `#[cfg]` 分岐が対称に書けるようにすること。

- [ ] **Step 1: native_voice.rs を読んで契約を書き出す**

`NativeVoice::start` / `stop` / `set_volume` / `statistics` のシグネチャと、`audio_flink` から `AudioSharedMemoryReader` を開く手順、`target_milliseconds` / `io_frames` の意味を書き出す。Windows 版はこれと同じ契約にする。

- [ ] **Step 2: 失敗するテストを書く**

実デバイスが要るため、テストはフォーマット変換とバッファ計算に限定する:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_frame_count_honors_target_latency() {
        // 48kHz で 20ms を要求したら 960 フレーム
        assert_eq!(buffer_frame_count(48_000, 20.0), 960);
        // 端数は切り上げ (不足によるアンダーランを避ける)
        assert_eq!(buffer_frame_count(44_100, 20.0), 882);
    }

    #[test]
    fn volume_is_clamped_to_valid_range() {
        assert_eq!(clamp_volume(-1.0), 0.0);
        assert_eq!(clamp_volume(0.5), 0.5);
        assert_eq!(clamp_volume(3.0), 1.0);
    }
}
```

- [ ] **Step 3: テストが失敗することを確認**

Run: `cargo test -p cef-unity-client buffer_frame_count` (vcvars64 経由)
Expected: FAIL (関数が存在しない)

- [ ] **Step 4: 純粋関数を実装してテストを通す**

```rust
/// 要求レイテンシからバッファのフレーム数を求める。切り上げでアンダーランを避ける。
fn buffer_frame_count(sample_rate: u32, target_milliseconds: f32) -> u32 {
    let frames = (sample_rate as f32) * target_milliseconds / 1000.0;
    frames.ceil() as u32
}

fn clamp_volume(volume: f32) -> f32 {
    volume.clamp(0.0, 1.0)
}
```

- [ ] **Step 5: WASAPI レンダースレッドを実装**

`CoInitializeEx(COINIT_MULTITHREADED)` → `IMMDeviceEnumerator::GetDefaultAudioEndpoint(eRender, eConsole)` → `IAudioClient::Initialize` を `AUDCLNT_SHAREMODE_SHARED | AUDCLNT_STREAMFLAGS_EVENTCALLBACK` で開く。`SetEventHandle` したイベントを待ち、`GetCurrentPadding` で空きを求め、`IAudioRenderClient::GetBuffer` / `ReleaseBuffer` にリングから読んだ PCM を書く。フォーマットが CEF 側 (f32 / チャンネル数) と異なる場合は変換する。停止はフラグ + イベント signal でスレッドを抜けさせ join する。

- [ ] **Step 6: FFI の cfg を拡張**

`lib.rs` の `cef_unity_audio_native_start` / `stop` / `set_volume` / `statistics` の `#[cfg(target_os = "macos")]` ブロックに対応する Windows ブロックを追加する。`ClientBrowserInstance` の `native_voice` フィールドの `#[cfg]` も Windows を含める (型は `WasapiOutput`)。

- [ ] **Step 7: 全テストとビルド**

Run: `cargo test` および `cargo build --release` (vcvars64 経由)
Expected: 全パス

- [ ] **Step 8: 実機で音を確認**

`deploy.ps1 -Arch x64` の後、シーンの `_audioRenderer` を `Native` にして Play し、音が鳴ること・途切れないことを確認する。`cef_load_url` トグルに 440Hz トーンの data URI か音の出るページを渡して検証する。

- [ ] **Step 9: コミット**

```bash
git add cef-unity-rust/crates/client/src/wasapi_output.rs cef-unity-rust/crates/client/src/lib.rs cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs
git commit -m "feat(win): WASAPI によるネイティブ音声出力を追加する"
```

---

## Phase 6: ドキュメントの追従

### Task 9: 実装とドキュメントのずれを解消する

**Files:**
- Modify: `cef-unity-rust/CLAUDE.md`
- Modify: `docs/REFACTORING_REPORT.md`
- Modify: `CLAUDE.md` (ルート)

**修正すべき既知のずれ:**

1. `cef-unity-rust/CLAUDE.md` は Windows の GPU 同期を「client が `OpenSharedResource1` で開いて `ID3D11DeviceContext4::Wait` で同期する」とだけ書いており、**D3D12 経路 (`ID3D12CommandQueue::Wait`) の説明が無い**。Unity は既定で D3D12 を選ぶため、実際の主経路が書かれていない状態。両経路と、`UnityPluginLoad` が D3D11/D3D12 の両方を試して生きている方を使う設計を明記する。
2. 同ファイルに Windows のビルド手順はあるが、**Git Bash から `cargo` を叩くと `/usr/bin/link` が MSVC の `link.exe` を隠して失敗する**落とし穴が書かれていない。PowerShell + `vcvars64.bat` から実行する旨を追記する。
3. `docs/REFACTORING_REPORT.md` の **CS-10 (`useGpu` 変数が未使用) は既に修正済み** (`CefUnityBrowserSample.cs:214` は `useGpu: true` を直接渡している)。解決済みとして記す。
4. 同レポートの **CLI-12 / CLI-13 / IPC-7** は本計画の Task 4 / 1 / 2 で対応するので、対応状況を反映する。
5. Windows の機能差 (Task 6-8 で解消したもの/しないもの) を一覧できる記述を `cef-unity-rust/CLAUDE.md` に追加する。ARM64 は本計画のスコープ外なので「クロスビルドのみ・実行検証なし」「`PostProcessWindows` が x64 決め打ちのため ARM64 プレイヤービルドは未対応」を既知の制限として明記する。

- [ ] **Step 1: 各ファイルを修正**

上記 1〜5 を反映する。実装を読んで書くこと — 推測で書かない。

- [ ] **Step 2: 記述が実装と一致することを確認**

書いた内容について、対応するコード箇所を実際に開いて照合する。特に FFI 名・ファイルパス・行番号。

- [ ] **Step 3: コミット**

```bash
git add CLAUDE.md cef-unity-rust/CLAUDE.md docs/REFACTORING_REPORT.md
git commit -m "docs: Windows の GPU 経路・ビルド落とし穴・既知の制限を実装に追従させる"
```

---

## 完了条件

- `cargo test` (全クレート) と `dotnet test` が全パス
- Unity Editor で D3D11 / D3D12 の両経路が描画し、Console エラー 0
- Windows プレイヤーがビルドでき、起動して描画する
- Windows でネイティブ音声が鳴り、生スクロール入力が有効になる
- ドキュメントに実装と食い違う記述が残っていない
