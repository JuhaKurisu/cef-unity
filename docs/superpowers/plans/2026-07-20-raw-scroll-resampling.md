# 生スクロールイベント取得 + リサンプリング (C 案) 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** macOS の NSEvent ローカルモニタで量子化前の生スクロールイベントを取得し、Chromium LinearResampling 準拠のリサンプラで per-frame 均一・低遅延 (~5ms) のスクロールを実現する。

**Architecture:** ネイティブ層 (scroll_monitor.m + FFI、client dylib 同梱 = 音声 au_output と同パターン) → `IScrollEventSource` 抽象 → 共有 `ScrollResampler` (純 C#) → 既存 SendMouseWheel 経路。precise はリサンプラ、非 precise (ホイールノッチ) は既存 ScrollSmoother、未対応環境は現行フォールバック。

**Tech Stack:** Rust (cc + csbindgen) / Obj-C (AppKit NSEvent) / C# (Unity, NUnit EditMode)

**Spec:** `docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md`

## Global Constraints

- コミット author は Juha のみ。`Co-Authored-By` を付けない
- コード内コメントは日本語 (既存流儀)
- `uloop run-tests` はセキュリティブロック → EditMode テストは batchmode CLI: `/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -runTests -testPlatform EditMode -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject -testResults <xml> -logFile <log>` (Unity Editor 終了必須、exit 0=全パス)
- dylib 変更後は Unity Editor 再起動が必須 (Editor は dylib をロードしたまま保持する)
- `scroll_monitor.m` を後から変更する場合は `cargo clean -p cef-unity-client --release` が必要 (.o キャッシュ問題)。新規追加の初回ビルドは不要
- `deploy.sh` は `cef-unity-rust/` ディレクトリから実行 (release ビルド + dylib/server 配置)
- スクロール動作の検証は実ユーザーの手動スクロールで行う (programmatic 注入では CEF が paint しない計測の罠)
- スペック定数: SampleOffset=5ms / ExtrapolationCap=8ms / GraceTimeout=100ms / リング容量 256
- リポジトリは単一 git (root = /Users/juha/Documents/GitHub/cef-unity)。cargo build は NativeMethods.g.cs を cef-unity-csharp/ と cef-unity-unityproject/ の両方に再生成するので両方コミットする

## File Structure

| ファイル | 責務 |
|---|---|
| Create: `cef-unity-rust/crates/client/src/scroll_monitor.m` | NSEvent ローカルモニタ + リングバッファ (メインスレッド専用) |
| Create: `cef-unity-rust/crates/client/src/scroll_monitor.rs` | .m の Rust バインディング (unsafe extern) |
| Modify: `cef-unity-rust/crates/client/src/lib.rs` | `CefScrollEvent` repr(C) + FFI 4 本 (csbindgen 対象) |
| Modify: `cef-unity-rust/crates/client/build.rs` | cc::Build 追加 + AppKit リンク |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollInputEvent.cs` | struct + enum + `IScrollEventSource` |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollResampler.cs` | リサンプラ本体 (純 C#) |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/MacNativeScrollSource.cs` | FFI 薄ラッパ |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollResamplerTests.cs` | EditMode テスト 10 件 |
| Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs` | SetupScrollInput / TickNativeScroll / ゲート / Reset / Dispose |

---

### Task 1: ネイティブ層 (scroll_monitor)

**Files:**
- Create: `cef-unity-rust/crates/client/src/scroll_monitor.m`
- Create: `cef-unity-rust/crates/client/src/scroll_monitor.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs`
- Modify: `cef-unity-rust/crates/client/build.rs`

**Interfaces:**
- Produces (Task 3 が C# から使用、csbindgen が `NativeMethods.g.cs` に生成):
  - `cef_scroll_monitor_start() -> i32` (1=成功 0=失敗)
  - `cef_scroll_monitor_stop()`
  - `cef_scroll_monitor_poll(out: *mut CefScrollEvent, max: i32) -> i32` (新着イベント数)
  - `cef_scroll_monitor_now() -> f64` (イベントと同一クロックの現在秒)
  - `CefScrollEvent { timestamp: f64, dx: f32, dy: f32, phase: u8, precise: u8 }` — phase: 0=None 1=GestureBegan 2=GestureChanged 3=GestureEnded 4=MomentumBegan 5=MomentumChanged 6=MomentumEnded 7=Cancelled

- [ ] **Step 1: scroll_monitor.m を作成**

`cef-unity-rust/crates/client/src/scroll_monitor.m`:

```objc
// NSEvent ローカルモニタでスクロールイベントを収集し、Unity 側の毎フレーム poll に渡す。
// スレッドモデル: モニタハンドラは AppKit メインスレッドで発火し、Unity スクリプト
// (poll 呼び出し側) も同じメインスレッドで動く。イベント配送はランループ上 (スクリプト
// 実行中には起きない) ため、リングはロック無しの単純配列で安全。
// 権限不要 (自アプリ宛イベントのみ)。イベントは素通し (return event) し通常配送を妨げない。
#import <AppKit/AppKit.h>
#import <string.h>

typedef struct {
    double timestamp;   // NSEvent.timestamp (起動からの秒)
    float dx, dy;       // scrollingDeltaX/Y (precise ならピクセル精度)
    uint8_t phase;      // 下の phase_of() 参照 (CefScrollEvent.phase と同一値)
    uint8_t precise;    // 1 = hasPreciseScrollingDeltas
} scroll_event_t;

#define RING_CAP 256
static scroll_event_t g_ring[RING_CAP];
static int g_count = 0;
static id g_monitor = nil;

static uint8_t phase_of(NSEvent *e) {
    NSEventPhase m = e.momentumPhase;
    if (m == NSEventPhaseBegan) return 4;
    if (m == NSEventPhaseChanged) return 5;
    if (m == NSEventPhaseEnded) return 6;
    if (m == NSEventPhaseCancelled) return 7;
    NSEventPhase p = e.phase;
    if (p == NSEventPhaseBegan) return 1;
    if (p == NSEventPhaseChanged) return 2;
    if (p == NSEventPhaseEnded) return 3;
    if (p == NSEventPhaseCancelled) return 7;
    return 0;
}

int cef_scroll_monitor_start_impl(void) {
    if (g_monitor != nil) return 1;
    if (NSApp == nil) return 0; // ヘッドレス (batchmode 等) → フォールバックさせる
    g_monitor = [NSEvent addLocalMonitorForEventsMatchingMask:NSEventMaskScrollWheel
                                                      handler:^NSEvent *(NSEvent *e) {
        if (g_count == RING_CAP) {
            // 飽和 (poll は毎フレームなので実質発生しない): 最古を捨てる
            memmove(g_ring, g_ring + 1, (RING_CAP - 1) * sizeof(scroll_event_t));
            g_count--;
        }
        scroll_event_t *s = &g_ring[g_count++];
        s->timestamp = e.timestamp;
        s->dx = (float)e.scrollingDeltaX;
        s->dy = (float)e.scrollingDeltaY;
        s->phase = phase_of(e);
        s->precise = e.hasPreciseScrollingDeltas ? 1 : 0;
        return e; // 素通し
    }];
    return g_monitor != nil ? 1 : 0;
}

void cef_scroll_monitor_stop_impl(void) {
    if (g_monitor != nil) {
        [NSEvent removeMonitor:g_monitor];
        g_monitor = nil;
    }
    g_count = 0;
}

int cef_scroll_monitor_poll_impl(scroll_event_t *out, int max) {
    int n = g_count < max ? g_count : max;
    memcpy(out, g_ring, (size_t)n * sizeof(scroll_event_t));
    if (n < g_count)
        memmove(g_ring, g_ring + n, (size_t)(g_count - n) * sizeof(scroll_event_t));
    g_count -= n;
    return n;
}

double cef_scroll_monitor_now_impl(void) {
    // NSEvent.timestamp と同一基準 (起動からの秒)
    return [[NSProcessInfo processInfo] systemUptime];
}
```

- [ ] **Step 2: scroll_monitor.rs を作成**

`cef-unity-rust/crates/client/src/scroll_monitor.rs`:

```rust
//! scroll_monitor.m (NSEvent ローカルモニタ) の Rust バインディング。
//! Unity メインスレッド == AppKit メインスレッドで poll する前提 (ロック無し)。
//! FFI 公開は lib.rs 側 (csbindgen が lib.rs のみを走査するため)。

/// scroll_monitor.m の scroll_event_t / lib.rs の CefScrollEvent と同一レイアウト。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawScrollEvent {
    pub timestamp: f64,
    pub dx: f32,
    pub dy: f32,
    pub phase: u8,
    pub precise: u8,
}

unsafe extern "C" {
    pub fn cef_scroll_monitor_start_impl() -> i32;
    pub fn cef_scroll_monitor_stop_impl();
    pub fn cef_scroll_monitor_poll_impl(out: *mut RawScrollEvent, max: i32) -> i32;
    pub fn cef_scroll_monitor_now_impl() -> f64;
}
```

- [ ] **Step 3: lib.rs に FFI エクスポートを追加**

`cef-unity-rust/crates/client/src/lib.rs` の既存 `mod` 宣言群 (ファイル冒頭付近、`mod au_output;` 等がある場所) に追加:

```rust
#[cfg(target_os = "macos")]
mod scroll_monitor;
```

同ファイルの既存 FFI エクスポート群の末尾 (例: `cef_unity_shutdown` の後) に追加:

```rust
/// 生スクロールイベント (scroll_monitor.m / C# 側と同一レイアウト)。
/// phase: 0=None 1=GestureBegan 2=GestureChanged 3=GestureEnded
///        4=MomentumBegan 5=MomentumChanged 6=MomentumEnded 7=Cancelled
#[repr(C)]
pub struct CefScrollEvent {
    pub timestamp: f64,
    pub dx: f32,
    pub dy: f32,
    pub phase: u8,
    pub precise: u8,
}

/// NSEvent スクロールモニタを開始する。1=成功 0=失敗 (ヘッドレス等)。
/// macOS 以外は常に 0 (呼び出し側がフォールバックする)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_start() -> i32 {
    #[cfg(target_os = "macos")]
    {
        unsafe { scroll_monitor::cef_scroll_monitor_start_impl() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_stop() {
    #[cfg(target_os = "macos")]
    unsafe {
        scroll_monitor::cef_scroll_monitor_stop_impl()
    }
}

/// 新着イベントを out に書き、件数を返す。毎フレーム呼ぶこと (リング鮮度維持)。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_poll(out: *mut CefScrollEvent, max: i32) -> i32 {
    #[cfg(target_os = "macos")]
    {
        unsafe {
            scroll_monitor::cef_scroll_monitor_poll_impl(
                out as *mut scroll_monitor::RawScrollEvent,
                max,
            )
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (out, max);
        0
    }
}

/// イベント timestamp と同一クロック (起動からの秒) の現在時刻。リサンプル基準用。
#[unsafe(no_mangle)]
pub extern "C" fn cef_scroll_monitor_now() -> f64 {
    #[cfg(target_os = "macos")]
    {
        unsafe { scroll_monitor::cef_scroll_monitor_now_impl() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0.0
    }
}
```

- [ ] **Step 4: build.rs に cc エントリと AppKit リンクを追加**

`cef-unity-rust/crates/client/build.rs` の `#[cfg(target_os = "macos")]` ブロック内、既存の `cc::Build` 群の後に追加:

```rust
        cc::Build::new()
            .file("src/scroll_monitor.m")
            .flag("-fobjc-arc")
            .compile("scroll_monitor");
        println!("cargo:rustc-link-lib=framework=AppKit");
```

- [ ] **Step 5: ビルドしてバインディング生成を確認**

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust
cargo build -p cef-unity-client
grep -c "cef_scroll_monitor" ../cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs
grep -c "CefScrollEvent" ../cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs
```

期待: ビルド成功、grep がそれぞれ 4 以上 / 1 以上を返す (バインディングが生成された)

- [ ] **Step 6: cargo test でリグレッション確認**

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust
cargo test -p cef-unity-client 2>&1 | tail -5
```

期待: 既存テストが全てパス (新規テストはなし。モニタは AppKit ランループが必要なため実機検証は Task 4)

注意: CEF 統合テストの罠 — 複数の cargo test を並行実行しない。panic 残留プロセスがあれば `pkill -f cef-unity-server`

- [ ] **Step 7: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-rust/crates/client/src/scroll_monitor.m \
        cef-unity-rust/crates/client/src/scroll_monitor.rs \
        cef-unity-rust/crates/client/src/lib.rs \
        cef-unity-rust/crates/client/build.rs \
        cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs \
        cef-unity-csharp/Interop/NativeMethods.g.cs
git commit -m "feat: NSEvent スクロールモニタ (生イベント取得の FFI, C案ネイティブ層)"
```

---

### Task 2: C# 抽象 + ScrollResampler (TDD)

**Files:**
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollInputEvent.cs`
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollResampler.cs`
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollResamplerTests.cs`

**Interfaces:**
- Produces (Task 3 が使用):
  - `namespace CefUnity.Runtime` / `enum ScrollPhase : byte { None=0, GestureBegan=1, GestureChanged=2, GestureEnded=3, MomentumBegan=4, MomentumChanged=5, MomentumEnded=6, Cancelled=7 }`
  - `struct ScrollInputEvent { double Timestamp; float DxPx, DyPx; bool Precise; ScrollPhase Phase; }`
  - `interface IScrollEventSource : IDisposable { bool Start(); int Poll(ScrollInputEvent[] buffer); double Now { get; } }`
  - `sealed class ScrollResampler { void AddEvent(in ScrollInputEvent e); void Tick(double now, out int dx, out int dy); void Reset(); bool IsActive { get; } }`

- [ ] **Step 1: 抽象定義ファイルを作成**

`cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollInputEvent.cs`:

```csharp
namespace CefUnity.Runtime
{
    /// <summary>スクロールジェスチャの局面 (macOS NSEventPhase 相当の抽象)。</summary>
    public enum ScrollPhase : byte
    {
        None = 0,
        GestureBegan = 1,
        GestureChanged = 2,
        GestureEnded = 3,
        MomentumBegan = 4,
        MomentumChanged = 5,
        MomentumEnded = 6,
        Cancelled = 7,
    }

    /// <summary>量子化前の生スクロールイベント 1 件。</summary>
    public struct ScrollInputEvent
    {
        /// <summary>ソース固有クロックの発生時刻 (秒)。IScrollEventSource.Now と同一基準。</summary>
        public double Timestamp;
        public float DxPx;
        public float DyPx;
        /// <summary>true = ピクセル精度 (トラックパッド)、false = ライン単位 (ホイールノッチ)。</summary>
        public bool Precise;
        public ScrollPhase Phase;
    }

    /// <summary>
    ///     プラットフォーム別の生スクロールイベント供給源。
    ///     Windows (WndProc サブクラス化) / Linux (XInput2) も本インターフェースで追加する。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public interface IScrollEventSource : System.IDisposable
    {
        /// <summary>取得を開始する。false = 使用不可 (呼び出し側はフォールバック)。</summary>
        bool Start();

        /// <summary>新着イベントを buffer に書き込み、件数を返す。毎フレーム呼ぶこと。</summary>
        int Poll(ScrollInputEvent[] buffer);

        /// <summary>イベントと同一クロックの現在時刻 (秒)。</summary>
        double Now { get; }
    }
}
```

- [ ] **Step 2: 失敗するテストを書く**

`cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollResamplerTests.cs`:

```csharp
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="ScrollResampler" /> の単体テスト。合成イベント列で
    ///     「per-frame 均一化・補間/外挿境界・momentum 終端の即時停止・総量保存」を検証する。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public class ScrollResamplerTests
    {
        private const double E = 1.0 / 120.0; // 120Hz イベント間隔
        private const double F = 1.0 / 60.0;  // 60fps フレーム間隔

        private static ScrollInputEvent Ev(double t, float dy, ScrollPhase phase = ScrollPhase.MomentumChanged)
            => new ScrollInputEvent { Timestamp = t, DyPx = dy, Precise = true, Phase = phase };

        // ---- イベント 1 点では補間せず即時排出 (低遅延スタート) ----

        [Test]
        public void SingleEvent_EmitsImmediately()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 30f));
            r.Tick(0.006, out _, out var dy);
            Assert.AreEqual(30, dy);
        }

        // ---- 均一 120Hz ストリーム → 追いつき後は毎フレームちょうど均一排出 ----

        [Test]
        public void SteadyStream_UniformPerFrameOutput()
        {
            var r = new ScrollResampler();
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var k = 0; k < 30; k++)
            {
                var now = 0.02 + k * F;
                while (eventIndex * E <= now)
                {
                    r.AddEvent(Ev(eventIndex * E, 5f));
                    eventIndex++;
                }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
            }
            // 600px/s の均一入力 → 初回フレームの追いつき以降は毎フレーム 10px ちょうど
            for (var k = 1; k < emitted.Count; k++)
                Assert.AreEqual(10, emitted[k], $"frame {k}");
        }

        // ---- イベントタイミングに ±3ms のジッター → 排出は準均一のまま ----

        [Test]
        public void JitteredTimestamps_OutputStaysNearUniform()
        {
            var r = new ScrollResampler();
            double[] jitter = { 0, +0.003, -0.002, +0.001, -0.003, +0.002 };
            var emitted = new System.Collections.Generic.List<int>();
            var eventIndex = 0;
            for (var k = 0; k < 24; k++)
            {
                var now = 0.02 + k * F;
                while (true)
                {
                    var t = eventIndex * E + jitter[eventIndex % jitter.Length];
                    if (t > now) break;
                    r.AddEvent(Ev(t, 5f));
                    eventIndex++;
                }
                r.Tick(now, out _, out var dy);
                emitted.Add(dy);
            }
            for (var k = 2; k < emitted.Count; k++)
                Assert.That(emitted[k], Is.InRange(5, 15), $"frame {k}");
        }

        // ---- momentum 終端: 残差を即時排出して停止 (浮遊しない) ----

        [Test]
        public void MomentumEnded_FlushesResidualThenStops()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f));
            r.Tick(E + 0.005, out _, out var d1);
            r.AddEvent(Ev(2 * E, 0f, ScrollPhase.MomentumEnded));
            r.Tick(2 * E + 0.005, out _, out var d2);
            Assert.AreEqual(20, d1 + d2, "終端までの総量が排出される");
            Assert.IsFalse(r.IsActive);
            r.Tick(2 * E + 0.005 + F, out _, out var d3);
            Assert.AreEqual(0, d3, "終端後は排出なし");
        }

        // ---- 終端イベント取り逃し → 100ms グレースで残差排出 ----

        [Test]
        public void GraceTimeout_FlushesResidual()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f));
            r.Tick(0.005 + E * 0.5, out _, out var d1); // sample=E/2 → 補間で 15 排出
            r.Tick(E + 0.150, out _, out var d2);        // グレース超過 → 残差 5
            Assert.AreEqual(20, d1 + d2);
            Assert.IsFalse(r.IsActive);
        }

        // ---- 外挿は 8ms で頭打ち、終端フラッシュで巻き戻さない ----

        [Test]
        public void Extrapolation_CappedAndNoBackwardFlush()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.000, 10f));
            r.AddEvent(Ev(E, 10f)); // 速度 1200px/s
            r.Tick(E + 0.005 + 0.050, out _, out var d1);
            // 外挿は cap=8ms まで: 20 + 1200*0.008 = 29.6 → 30
            Assert.AreEqual(30, d1);
            r.Tick(E + 0.200, out _, out var d2);
            // サンプルは最終位置 20 を追い越している → 巻き戻さない
            Assert.AreEqual(0, d2);
            Assert.IsFalse(r.IsActive);
        }

        // ---- 方向反転でも正味総量が保存される ----

        [Test]
        public void Reversal_NetTotalConserved()
        {
            var r = new ScrollResampler();
            var t = 0.0;
            for (var i = 0; i < 6; i++) { r.AddEvent(Ev(t, +10f)); t += E; }
            for (var i = 0; i < 6; i++) { r.AddEvent(Ev(t, -20f)); t += E; }
            r.AddEvent(Ev(t, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var k = 0; k < 20; k++)
            {
                r.Tick(0.02 + k * F, out _, out var dy);
                total += dy;
            }
            Assert.AreEqual(-60, total, "正味 +60-120 = -60");
            Assert.IsFalse(r.IsActive);
        }

        // ---- 小数 delta の端数繰り越しで総量保存 ----

        [Test]
        public void FractionCarry_ConservesFractionalDeltas()
        {
            var r = new ScrollResampler();
            var t = 0.0;
            for (var i = 0; i < 10; i++) { r.AddEvent(Ev(t, 1.5f)); t += E; }
            r.AddEvent(Ev(t, 0f, ScrollPhase.MomentumEnded));
            var total = 0;
            for (var k = 0; k < 10; k++) { r.Tick(0.02 + k * F, out _, out var dy); total += dy; }
            Assert.AreEqual(15, total);
        }

        // ---- momentum 終端の Tick を挟まず新ジェスチャ開始 → 残差は引き継がれる ----

        [Test]
        public void NewGestureAfterMomentumEnd_ContinuesCleanly()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 10f));
            r.AddEvent(Ev(E, 10f));
            r.AddEvent(Ev(2 * E, 0f, ScrollPhase.MomentumEnded));
            r.AddEvent(Ev(3 * E, 7f, ScrollPhase.GestureBegan));
            var total = 0;
            for (var k = 0; k < 10; k++) { r.Tick(0.03 + k * F, out _, out var dy); total += dy; }
            r.AddEvent(Ev(0.2, 0f, ScrollPhase.MomentumEnded));
            r.Tick(0.25, out _, out var last);
            Assert.AreEqual(27, total + last, "旧 20 + 新 7 が全て排出される");
        }

        // ---- Reset で全状態破棄 ----

        [Test]
        public void Reset_ClearsState()
        {
            var r = new ScrollResampler();
            r.AddEvent(Ev(0.0, 100f));
            r.Reset();
            Assert.IsFalse(r.IsActive);
            r.Tick(0.05, out _, out var dy);
            Assert.AreEqual(0, dy);
        }
    }
}
```

- [ ] **Step 3: batchmode でコンパイル失敗 (RED) を確認**

```bash
pgrep -x Unity >/dev/null && { pkill -x Unity; sleep 3; }
/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -runTests -testPlatform EditMode \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -testResults /tmp/resampler_red.xml -logFile /tmp/resampler_red.log
echo "exit=$?"
grep -c "error CS0246" /tmp/resampler_red.log
```

期待: exit 非 0、CS0246 (ScrollResampler が見つからない) が出る

- [ ] **Step 4: ScrollResampler を実装**

`cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/ScrollResampler.cs`:

```csharp
using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     生スクロールイベント列を「フレーム時刻」へ再標本化する (Chromium
    ///     LinearResampling 準拠)。イベントから累積位置 P(t) を構築し、毎フレーム
    ///     sampleTime = now − SampleOffset の P を直近2イベントの線形補間 (イベント間)
    ///     または線形外挿 (最終イベント以後、上限 ExtrapolationCap) で求め、前回サンプル
    ///     との差分を int px で排出する (端数繰り越しで総量保存)。momentum 終端では残差を
    ///     即時排出して停止する (A案 ScrollSmoother の「停止後の浮遊」が構造的に生じない)。
    ///     純 C# (Unity API 非依存)。時刻はイベントと同一クロック (秒) を呼び出し側が渡す。
    ///     設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
    /// </summary>
    public sealed class ScrollResampler
    {
        /// <summary>サンプル時刻のオフセット (秒)。now からこの分だけ過去を標本化する。</summary>
        public const double SampleOffset = 0.005;

        /// <summary>最終イベントからの外挿上限 (秒)。超えた分は保持 (オーバーシュート防止)。</summary>
        public const double ExtrapolationCap = 0.008;

        /// <summary>無イベントでジェスチャ終端とみなすグレース (秒)。</summary>
        public const double GraceTimeout = 0.100;

        // 直近2イベントの (時刻, 累積位置)。_count は保持点数 (0/1/2)。
        private double _t0, _t1;
        private double _p0X, _p0Y, _p1X, _p1Y;
        private int _count;

        // 前回サンプル位置と、int 排出の端数繰り越し。
        private double _sampX, _sampY;
        private double _fracX, _fracY;

        // momentum Ended/Cancelled 受信済み。次の Tick で残差を排出して停止する。
        private bool _ended;

        /// <summary>追跡中のジェスチャがあるか。</summary>
        public bool IsActive => _count > 0;

        public void Reset()
        {
            _count = 0;
            _t0 = _t1 = 0;
            _p0X = _p0Y = _p1X = _p1Y = 0;
            _sampX = _sampY = 0;
            _fracX = _fracY = 0;
            _ended = false;
        }

        /// <summary>イベントを取り込む (delta は view px スケール済みであること)。</summary>
        public void AddEvent(in ScrollInputEvent e)
        {
            if (_ended)
            {
                // 前ジェスチャの終端 Tick を挟まず新ジェスチャが始まった:
                // 残差を端数バッファへ退避してから履歴を作り直す (排出は次の Tick)。
                FlushResidualToFraction();
            }
            Accumulate(e);
            if (e.Phase == ScrollPhase.MomentumEnded || e.Phase == ScrollPhase.Cancelled)
                _ended = true;
        }

        private void Accumulate(in ScrollInputEvent e)
        {
            if (_count == 0)
            {
                _t1 = e.Timestamp;
                // 前回サンプル位置から連続に開始する (位置ジャンプ防止)。
                _p1X = _sampX + e.DxPx;
                _p1Y = _sampY + e.DyPx;
                _count = 1;
                return;
            }
            if (e.Timestamp <= _t1)
            {
                // 同時刻イベント (同フレーム複数イベント等) は最新点へ合算 (0 除算回避)。
                _p1X += e.DxPx;
                _p1Y += e.DyPx;
                return;
            }
            _t0 = _t1;
            _p0X = _p1X;
            _p0Y = _p1Y;
            _t1 = e.Timestamp;
            _p1X += e.DxPx;
            _p1Y += e.DyPx;
            _count = 2;
        }

        /// <summary>
        ///     残差 (最終イベント位置 − 前回サンプル) を端数バッファへ移し、履歴をクリアする。
        ///     外挿でサンプルが最終位置を追い越していた場合 (残差が直近の進行方向と逆) は
        ///     捨てる — 終端での「巻き戻し」を防ぐ。
        /// </summary>
        private void FlushResidualToFraction()
        {
            var rx = _p1X - _sampX;
            var ry = _p1Y - _sampY;
            var dirX = _p1X - _p0X;
            var dirY = _p1Y - _p0Y;
            if (_count < 2 || rx * dirX >= 0) _fracX += rx;
            if (_count < 2 || ry * dirY >= 0) _fracY += ry;
            _count = 0;
            _t0 = _t1 = 0;
            _p0X = _p0Y = _p1X = _p1Y = 0;
            _sampX = _sampY = 0;
            _ended = false;
        }

        /// <summary>1 フレーム分の排出量を計算する。now はイベントと同一クロック (秒)。</summary>
        public void Tick(double now, out int dx, out int dy)
        {
            if (_count > 0)
            {
                if (_ended || now - _t1 > GraceTimeout)
                {
                    FlushResidualToFraction();
                }
                else
                {
                    var sampleTime = now - SampleOffset;
                    double sx, sy;
                    if (_count < 2 || sampleTime >= _t1)
                    {
                        if (_count == 2 && sampleTime > _t1)
                        {
                            // 最終イベント以後: 直近2点の速度で外挿 (上限 cap)。
                            var dt = Math.Min(sampleTime - _t1, ExtrapolationCap);
                            var span = _t1 - _t0;
                            sx = _p1X + (_p1X - _p0X) / span * dt;
                            sy = _p1Y + (_p1Y - _p0Y) / span * dt;
                        }
                        else
                        {
                            // 補間に足る2点が無い: 最新位置をそのまま使う (即時排出)。
                            sx = _p1X;
                            sy = _p1Y;
                        }
                    }
                    else if (sampleTime <= _t0)
                    {
                        sx = _p0X;
                        sy = _p0Y;
                    }
                    else
                    {
                        // イベント間: 線形補間 (リサンプリングの本体)。
                        var a = (sampleTime - _t0) / (_t1 - _t0);
                        sx = _p0X + (_p1X - _p0X) * a;
                        sy = _p0Y + (_p1Y - _p0Y) * a;
                    }
                    _fracX += sx - _sampX;
                    _fracY += sy - _sampY;
                    _sampX = sx;
                    _sampY = sy;
                }
            }
            dx = TakeInt(ref _fracX);
            dy = TakeInt(ref _fracY);
        }

        private static int TakeInt(ref double frac)
        {
            var v = (int)Math.Round(frac);
            frac -= v;
            return v;
        }
    }
}
```

- [ ] **Step 5: batchmode でテスト全パス (GREEN) を確認**

```bash
pgrep -x Unity >/dev/null && { pkill -x Unity; sleep 3; }
/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -runTests -testPlatform EditMode \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -testResults /tmp/resampler_green.xml -logFile /tmp/resampler_green.log
echo "exit=$?"
grep -o 'total="[0-9]*" passed="[0-9]*" failed="[0-9]*"' /tmp/resampler_green.xml | head -1
```

期待: exit=0、total="29" passed="29" failed="0" (既存 19 + 新 10)

- [ ] **Step 6: コミット (.meta 含む — ScrollInput ディレクトリの .meta も)**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput \
        cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput.meta \
        cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollResamplerTests.cs \
        cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollResamplerTests.cs.meta
git commit -m "feat: IScrollEventSource 抽象 + ScrollResampler (Chromium 流リサンプラ) + テスト10件"
```

---

### Task 3: MacNativeScrollSource + サンプル統合

**Files:**
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/MacNativeScrollSource.cs`
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs`

**Interfaces:**
- Consumes: Task 1 の FFI (`NativeMethods.cef_scroll_monitor_*`, `CefScrollEvent`)、Task 2 の抽象と `ScrollResampler`
- Produces: なし (最終統合)

- [ ] **Step 1: MacNativeScrollSource を作成**

`cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/MacNativeScrollSource.cs`:

```csharp
using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     macOS の NSEvent ローカルモニタ (client dylib 内 scroll_monitor.m) から
    ///     生スクロールイベントを取得する <see cref="IScrollEventSource" /> 実装。
    ///     Unity メインスレッド == AppKit メインスレッドで Poll する前提。
    /// </summary>
    public sealed class MacNativeScrollSource : IScrollEventSource
    {
        // NSEvent.scrollingDelta の符号は現行 Input.mouseScrollDelta 経路と同じ想定。
        // 実機検証 (Task 4) で逆だった場合はここを -1 にする。
        private const float SignX = 1f;
        private const float SignY = 1f;

        private bool _started;
        private readonly CefScrollEvent[] _native = new CefScrollEvent[256];

        public bool Start()
        {
            _started = NativeMethods.cef_scroll_monitor_start() != 0;
            return _started;
        }

        public unsafe int Poll(ScrollInputEvent[] buffer)
        {
            if (!_started) return 0;
            int n;
            fixed (CefScrollEvent* p = _native)
            {
                n = NativeMethods.cef_scroll_monitor_poll(p, Math.Min(_native.Length, buffer.Length));
            }
            for (var i = 0; i < n; i++)
            {
                buffer[i] = new ScrollInputEvent
                {
                    Timestamp = _native[i].timestamp,
                    DxPx = _native[i].dx * SignX,
                    DyPx = _native[i].dy * SignY,
                    Precise = _native[i].precise != 0,
                    Phase = (ScrollPhase)_native[i].phase,
                };
            }
            return n;
        }

        public double Now => NativeMethods.cef_scroll_monitor_now();

        public void Dispose()
        {
            if (!_started) return;
            _started = false;
            NativeMethods.cef_scroll_monitor_stop();
        }
    }
}
```

注意: `NativeMethods` / `CefScrollEvent` は `CefUnity` 名前空間 (csbindgen 生成)。`CefUnity.Runtime` からは `using` 不要で `CefUnity.` 無しに解決される (親名前空間)。もしフィールド名の大文字小文字が生成コードと違う場合 (`timestamp` 等は Rust 側の小文字のまま生成される)、生成された `NativeMethods.g.cs` の `CefScrollEvent` 定義に合わせること。

- [ ] **Step 2: CefUnityBrowserSample にフィールドを追加**

`ScrollSmoother` フィールド群 (`private readonly ScrollSmoother _scrollSmoother = ...` とその上のコメント) の直後に追加:

```csharp
        // 生スクロール入力 (C案): native ソース + リサンプラ。_scrollSource が null なら
        // フォールバック (現行の Input.mouseScrollDelta → ScrollSmoother 経路)。
        // 設計: docs/superpowers/specs/2026-07-20-raw-scroll-resampling-design.md
        private IScrollEventSource _scrollSource;
        private readonly ScrollResampler _scrollResampler = new ScrollResampler();
        private readonly ScrollInputEvent[] _scrollEventBuf = new ScrollInputEvent[256];
```

- [ ] **Step 3: SetupScrollInput メソッドを追加し Start() から呼ぶ**

`Start()` 内の `SetupAudioOutput();` の直後に追加:

```csharp
                SetupScrollInput();
```

`Start()` メソッドの直後 (クラス内) に追加:

```csharp
        /// <summary>
        ///     生スクロール入力 (C案) の初期化。native ソースが使えれば有効化し、以後
        ///     Input.mouseScrollDelta は読まない (二重計上防止)。失敗時は現行経路のまま。
        /// </summary>
        private void SetupScrollInput()
        {
#if UNITY_STANDALONE_OSX || UNITY_EDITOR_OSX
#if UNITY_EDITOR || DEVELOPMENT_BUILD
            // 開発トグル: cef_scroll_legacy で強制フォールバック (A/C 体感比較用)。
            if (System.IO.File.Exists(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_legacy")))
            {
                CefLog.Log("[CefUnity] scroll: legacy mode (cef_scroll_legacy)");
                return;
            }
#endif
            var src = new MacNativeScrollSource();
            if (src.Start())
            {
                _scrollSource = src;
                CefLog.Log("[CefUnity] scroll: native NSEvent source active");
            }
            else
            {
                src.Dispose();
                CefLog.Log("[CefUnity] scroll: native source unavailable — frame-polled fallback");
            }
#endif
        }
```

- [ ] **Step 4: HandleMouseInput の wheel 蓄積を native モード時にスキップ**

`HandleMouseInput()` 末尾の以下:

```csharp
            var scroll = Input.mouseScrollDelta;
            if (scroll.y != 0f || scroll.x != 0f)
            {
                // ステップ→ピクセル変換。_resolutionScale で view (CSS px) が広がった分も
                // 掛けて、画面上の体感スクロール速度を scale に依らず一定に保つ
                // (マウス座標は既に scale 込みの view 座標へ変換している)。
                // 送信は即時ではなく ScrollSmoother へ蓄積し、OnEarlyUpdateLast の
                // TickScrollSmoother が毎フレーム均一化して排出する。
                _scrollSmoother.AddInput(
                    scroll.x * WheelPixelsPerStep * _resolutionScale,
                    scroll.y * WheelPixelsPerStep * _resolutionScale);
            }
```

を以下に置換:

```csharp
            // native ソース有効時は生イベント経路 (TickNativeScroll) が担うため、
            // フレーム量子化された Input.mouseScrollDelta は読まない (二重計上防止)。
            if (_scrollSource == null)
            {
                var scroll = Input.mouseScrollDelta;
                if (scroll.y != 0f || scroll.x != 0f)
                {
                    // ステップ→ピクセル変換。_resolutionScale で view (CSS px) が広がった分も
                    // 掛けて、画面上の体感スクロール速度を scale に依らず一定に保つ
                    // (マウス座標は既に scale 込みの view 座標へ変換している)。
                    // 送信は即時ではなく ScrollSmoother へ蓄積し、OnEarlyUpdateLast の
                    // TickScrollSmoother が毎フレーム均一化して排出する。
                    _scrollSmoother.AddInput(
                        scroll.x * WheelPixelsPerStep * _resolutionScale,
                        scroll.y * WheelPixelsPerStep * _resolutionScale);
                }
            }
```

- [ ] **Step 5: TickNativeScroll メソッドを追加**

`TickScrollSmoother()` メソッドの直前に追加:

```csharp
        /// <summary>
        ///     native スクロールソースの 1 フレーム処理。イベントを drain し、カーソルが
        ///     ブラウザ上のときだけ転送する (Editor で他ウィンドウ上のスクロールを拾わない)。
        ///     precise はリサンプラへ、非 precise (ホイールノッチ) は ScrollSmoother へ。
        /// </summary>
        private void TickNativeScroll()
        {
            if (_scrollSource == null || _browser == null) return;
            var n = _scrollSource.Poll(_scrollEventBuf);
            var overBrowser = TryGetBrowserCoord(out _, out _);
            if (overBrowser)
            {
                for (var i = 0; i < n; i++)
                {
                    ref var e = ref _scrollEventBuf[i];
                    if (e.Precise)
                    {
                        // precise delta は CSS px 相当 → view 座標へ _resolutionScale を掛ける。
                        var scaled = e;
                        scaled.DxPx *= _resolutionScale;
                        scaled.DyPx *= _resolutionScale;
                        _scrollResampler.AddEvent(in scaled);
                    }
                    else
                    {
                        // ノッチ (ライン単位) は ScrollSmoother でグライドさせる (Chrome 層3相当)。
                        _scrollSmoother.AddInput(
                            e.DxPx * WheelPixelsPerStep * _resolutionScale,
                            e.DyPx * WheelPixelsPerStep * _resolutionScale);
                    }
                }
            }
            _scrollResampler.Tick(_scrollSource.Now, out var dx, out var dy);
            if (dx == 0 && dy == 0) return;
            // 最後の有効マウス座標で送る。まだ一度も動いていなければ画面中央。
            var bx = _lastMouseX >= 0 ? _lastMouseX : _currentWidth / 2;
            var by = _lastMouseY >= 0 ? _lastMouseY : _currentHeight / 2;
            _browser.SendMouseWheel(bx, by, dx, dy, GetCefModifiers());
            _inputSentThisFrame = true;
#if UNITY_EDITOR || DEVELOPMENT_BUILD
            _frameSentDy = dy; // 分析用: リサンプル後の実送信量
#endif
        }
```

- [ ] **Step 6: OnEarlyUpdateLast に呼び出しを挿入**

`OnEarlyUpdateLast()` 内の以下:

```csharp
            // スクロール平滑の排出。BeginFrame#1 の前なので同フレームの paint に乗る。
            self.TickScrollSmoother();
```

を以下に置換:

```csharp
            // 生イベント経路 (C案): native ソースを drain し、リサンプラ排出を送る。
            self.TickNativeScroll();
            // スクロール平滑の排出 (非 precise / フォールバック)。BeginFrame#1 の前なので
            // 同フレームの paint に乗る。
            self.TickScrollSmoother();
```

- [ ] **Step 7: LoadUrl と OnDestroy に後始末を追加**

`LoadUrl` を以下に置換:

```csharp
        public void LoadUrl(string url)
        {
            // グライド途中の残距離/履歴を新ページへ流し込まない。
            _scrollSmoother.Reset();
            _scrollResampler.Reset();
            _browser.LoadUrl(url);
        }
```

`OnDestroy()` 内の `if (s_instance == this) s_instance = null;` の直後に追加:

```csharp
            _scrollSource?.Dispose();
            _scrollSource = null;
```

- [ ] **Step 8: batchmode でコンパイル+テスト確認**

```bash
pgrep -x Unity >/dev/null && { pkill -x Unity; sleep 3; }
/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -runTests -testPlatform EditMode \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -testResults /tmp/task3_tests.xml -logFile /tmp/task3_test.log
echo "exit=$?"
grep -o 'total="[0-9]*" passed="[0-9]*" failed="[0-9]*"' /tmp/task3_tests.xml | head -1
```

期待: exit=0、total="29" passed="29" failed="0"

- [ ] **Step 9: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/MacNativeScrollSource.cs \
        cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollInput/MacNativeScrollSource.cs.meta \
        cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs
git commit -m "feat: native スクロールソースをサンプルに統合 (C案: リサンプル経路 + legacy トグル)"
```

---

### Task 4: deploy + ビルド実測検証 (要ユーザー協力)

**Files:** 変更なし (検証のみ。符号が逆だった場合のみ `MacNativeScrollSource.cs` の SignY 修正)

**Interfaces:**
- Consumes: Task 1-3 の成果物、既存の計測基盤 (`cef_perf_probe` / `cef_scroll_legacy` / `/tmp/cef_heavy.html`)

- [ ] **Step 1: deploy.sh で release ビルド + dylib 配置**

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust
./deploy.sh
```

期待: エラーなし (dylib が Assets/CefUnity/Interop/Plugins/osx-arm64/ に配置され codesign される)

- [ ] **Step 2: 開発ビルドを作成**

```bash
pgrep -x Unity >/dev/null && { pkill -x Unity; sleep 3; }
/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -quit \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -executeMethod CefUnity.Editor.CefQuickBuild.BuildMac \
  -logFile /tmp/cef_build_c.log
echo "exit=$?"
grep -E "result=" /tmp/cef_build_c.log | head -2
```

期待: result=Succeeded

- [ ] **Step 3: 起動して native モードの有効化を確認**

```bash
rm -f "$TMPDIR/cef_scroll_legacy" "$TMPDIR/cef_perf.csv"
touch "$TMPDIR/cef_perf_probe"
echo "file:///tmp/cef_heavy.html" > "$TMPDIR/cef_load_url"
pkill -f "CefUnity.app/Contents/MacOS" 2>/dev/null; pkill -f "cef-unity-server" 2>/dev/null; sleep 1
open /Users/juha/Documents/GitHub/cef-unity/build-mac/CefUnity.app
sleep 30
grep "scroll:" "$HOME/Library/Logs/DefaultCompany/cef-unity-unityproject/Player.log" | tail -2
```

期待: `[CefUnity] scroll: native NSEvent source active`
(dev ビルドは初回起動が遅い — 30 秒待つこと)

- [ ] **Step 4: ユーザー検証① — 方向と基本動作**

ユーザーに依頼: 「トラックパッドで上下にスクロールしてください。**方向が逆に感じたら教えてください**」

- 方向が逆の場合: `MacNativeScrollSource.cs` の `SignY` を `-1f` に変更し、Task 3 Step 8 の要領で再ビルド・再確認し、`fix: native スクロール delta の符号を反転` でコミット

- [ ] **Step 5: ユーザー検証② — 体感 A/B (C 案 vs A 案)**

```bash
# A 案 (legacy) に切替えて比較 (要アプリ再起動 — SetupScrollInput は起動時のみ判定)
touch "$TMPDIR/cef_scroll_legacy"
pkill -f "CefUnity.app/Contents/MacOS"; sleep 1
open /Users/juha/Documents/GitHub/cef-unity/build-mac/CefUnity.app
# → ユーザーがスクロール。その後 rm -f "$TMPDIR/cef_scroll_legacy" して再起動で C 案に戻す
```

ユーザーに依頼: 「C 案 (native) と A 案 (legacy) を交互に試し、遅延感 (特にフリック停止時の浮遊) と滑らかさを比較してください」

成功基準 (スペック): **遅延感の解消 (τ 遅れ・停止後の浮遊感が消える) + 滑らかさが A 案以上**

- [ ] **Step 6: probe CSV で定量確認**

```bash
python3 - <<'PY'
import os, statistics as st
p = os.path.join(os.environ['TMPDIR'], 'cef_perf.csv')
rows = [l.split(',') for l in open(p) if l.strip()]
d = [(int(r[0]), float(r[1]), int(r[2]), int(r[3])) for r in rows if len(r) == 4]
runs, cur = [], []
for f, dt, afi, dy in d:
    if dy != 0: cur.append(dy)
    else:
        if len(cur) >= 5: runs.append(cur)
        cur = []
if len(cur) >= 5: runs.append(cur)
rough = []
for run in runs:
    for i in range(1, len(run) - 1):
        local = (abs(run[i-1]) + abs(run[i]) + abs(run[i+1])) / 3
        if local < 3: continue
        rough.append(abs(run[i] - (run[i-1] + run[i+1]) / 2) / local)
print(f"連続run={len(runs)} 評価点={len(rough)}")
if rough:
    print(f"正規化粗さ: median={st.median(rough):.3f} (A案 τ=45ms 実測 0.045 と比較)")
PY
```

期待: 正規化粗さが A 案実測と同等以下

- [ ] **Step 7: リリース経路のコンパイル検証**

新コードの dev トグル (`cef_scroll_legacy`) が #if の外に参照を残していないことを、リリース相当ビルドで確認する:

```bash
# CefQuickBuild.cs の options を一時的に BuildOptions.None に変更してビルド
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject
sed -i '' 's/options = BuildOptions.Development,/options = BuildOptions.None,/' Assets/CefUnity/Editor/CefQuickBuild.cs
pgrep -x Unity >/dev/null && { pkill -x Unity; sleep 3; }
/Applications/Unity/Hub/Editor/6000.3.8f1/Unity.app/Contents/MacOS/Unity -batchmode -quit \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -executeMethod CefUnity.Editor.CefQuickBuild.BuildMac -logFile /tmp/cef_build_release_c.log
echo "exit=$?"; grep -E "result=" /tmp/cef_build_release_c.log | head -1
# 検証後、必ず Development に戻す (この変更はコミットしない)
git checkout -- Assets/CefUnity/Editor/CefQuickBuild.cs
```

期待: result=Succeeded → 戻した後 `git status` で CefQuickBuild.cs がクリーン

- [ ] **Step 8: 検証結果の記録**

結果 (体感・符号・粗さ) をプロジェクトメモリ `scroll-stutter-diagnosis.md` に追記する (メモリは repo 外、コミット不要)
