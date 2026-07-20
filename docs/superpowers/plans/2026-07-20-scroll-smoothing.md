# スクロール入力平滑 (ScrollSmoother) 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** トラックパッド/ホイールの生 delta を指数追従で均一化して CEF に送り、per-frame スクロール量ジッター (CoV 0.82) によるカクツキを解消する。

**Architecture:** 純 C# クラス `ScrollSmoother` が「残距離への入力蓄積 → 毎フレーム指数追従排出」を担い、`CefUnityBrowserSample` は wheel 入力を蓄積に回し、`OnEarlyUpdateLast` の独立ステップで排出値を `SendMouseWheel` する。τ は temp ファイルで実行時チューニング可能 (暫定機構)。

**Tech Stack:** Unity 6 / C# / NUnit (EditMode, `Runtime.Tests` アセンブリ) / uloop CLI / batchmode ビルド (CefQuickBuild)

**Spec:** `docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md`

## Global Constraints

- コミット author は Juha のみ。`Co-Authored-By: Claude` を付けない
- コード内コメントは既存コードベースの流儀に合わせ日本語
- `uloop compile` / `uloop run-tests` は Unity Editor 起動が必要 (`uloop launch /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject`)。batchmode ビルドは逆に Editor 終了が必要
- 新規 .cs ファイルは Editor のインポートで .meta が生成される。**コミット時は .meta も必ず含める**
- スクロール paint の動作検証を programmatic 注入で行わない (ビルド窓が真にアクティブでないと CEF が paint しない計測の罠)。検証は実ユーザーの手動スクロール + probe CSV で行う
- 既存の temp ファイルトグル群 (`cef_scroll_test` / `cef_scroll_slow` / `cef_perf_probe` / `cef_novsync` 等) の挙動を壊さない

## File Structure

| ファイル | 責務 |
|---|---|
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollSmoother.cs` | 平滑ロジック本体 (純 C#、Unity API 非依存) |
| Create: `cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollSmootherTests.cs` | EditMode ユニットテスト |
| Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs` | 蓄積への切替・排出ステップ・τ 機構・Reset 統合 |

---

### Task 1: ScrollSmoother クラス + ユニットテスト (TDD)

**Files:**
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollSmootherTests.cs`
- Create: `cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollSmoother.cs`

**Interfaces:**
- Produces (Task 2 が依存):
  - `namespace CefUnity.Runtime` / `public sealed class ScrollSmoother`
  - `public void AddInput(float dxPx, float dyPx)` — 残距離に加算
  - `public void Tick(float dt, float tau, out int dx, out int dy)` — dt 秒経過分の排出量 (px int)。`tau <= 0` で従来挙動 (切り捨て+端数繰り越し即時排出)
  - `public void Reset()` — 残距離破棄
  - `public bool IsActive { get; }` — 残距離が残っているか

- [ ] **Step 1: Unity Editor を起動する**

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject && uloop launch
```

期待: Unity Editor が起動する (既に起動済みならフォーカスされる)

- [ ] **Step 2: 失敗するテストを書く**

`cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollSmootherTests.cs` を作成:

```csharp
using System;
using CefUnity.Runtime;
using NUnit.Framework;

namespace CefUnity.Runtime.Tests
{
    /// <summary>
    ///     <see cref="ScrollSmoother" /> の単体テスト。
    ///     生 wheel delta を残距離に蓄積し、毎フレーム指数追従で均一化排出する
    ///     平滑器が「総量保存・幾何減衰・方向反転・終端スナップ・平滑OFF互換」を
    ///     満たすことを検証する。
    ///     設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
    /// </summary>
    public class ScrollSmootherTests
    {
        private const float Dt60 = 1f / 60f;   // 60fps のフレーム時間
        private const float Tau = 0.045f;      // 既定の時定数 45ms

        // ---- 平滑 OFF (tau <= 0): 従来挙動 (int 切り捨て + 端数繰り越し) ----

        [Test]
        public void TauZero_EmitsImmediately_WithFractionCarry()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 100.7f);
            s.Tick(Dt60, 0f, out _, out var dy);
            Assert.AreEqual(100, dy, "切り捨てで 100 を即時排出");
            // 端数 0.7 が繰り越され、次の 0.5 と合算で 1.2 → 1 排出
            s.AddInput(0f, 0.5f);
            s.Tick(Dt60, 0f, out _, out dy);
            Assert.AreEqual(1, dy, "繰り越し端数 0.7 + 0.5 = 1.2 → 1");
        }

        // ---- 幾何減衰: 大入力が単調減少のグライドに分散される ----

        [Test]
        public void Smoothing_LargeInput_DecaysMonotonically()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1000f);
            s.Tick(Dt60, Tau, out _, out var e1);
            s.Tick(Dt60, Tau, out _, out var e2);
            s.Tick(Dt60, Tau, out _, out var e3);
            Assert.Greater(e1, e2);
            Assert.Greater(e2, e3);
            Assert.Greater(e3, 0);
            // τ=45ms, 60fps では初フレームで残りの約 31% が出る (食いつき確認)
            Assert.That(e1, Is.InRange(280, 340));
        }

        // ---- 総量保存: 整数入力なら排出合計が入力と厳密一致 ----

        [Test]
        public void Smoothing_IntegerInput_ConservesTotal()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 1000f);
            var total = 0;
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out _, out var dy);
                total += dy;
            }
            Assert.IsFalse(s.IsActive, "200 フレーム以内に排出し切る");
            Assert.AreEqual(1000, total);
        }

        // ---- 方向反転: 逆符号入力で残距離が相殺され、最終総量が一致 ----

        [Test]
        public void Smoothing_Reversal_NetTotalMatches()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 100f);
            var total = 0;
            s.Tick(Dt60, Tau, out _, out var dy);
            total += dy;
            s.AddInput(0f, -200f); // 反転 (残距離 ≈ 69 - 200 = -131)
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out _, out dy);
                total += dy;
            }
            Assert.AreEqual(-100, total, "正味 100 - 200 = -100");
        }

        // ---- 終端スナップ: 微小残距離は破棄され無限テールにならない ----

        [Test]
        public void Smoothing_TinyResidual_SnapsToZero()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 0.4f);
            s.Tick(Dt60, Tau, out _, out var dy);
            Assert.AreEqual(0, dy, "0.5px 未満は破棄");
            Assert.IsFalse(s.IsActive);
        }

        // ---- dt 非依存: 同じ実時間なら分割数に依らずほぼ同量を排出 ----

        [Test]
        public void Smoothing_DtInvariance_WithinTolerance()
        {
            var one = new ScrollSmoother();
            one.AddInput(0f, 1000f);
            one.Tick(1f / 30f, Tau, out _, out var single);

            var two = new ScrollSmoother();
            two.AddInput(0f, 1000f);
            two.Tick(Dt60, Tau, out _, out var a);
            two.Tick(Dt60, Tau, out _, out var b);

            Assert.That(single, Is.EqualTo(a + b).Within(2), "int 丸め分の誤差 ±2px まで許容");
        }

        // ---- X 軸も同一機構で処理される ----

        [Test]
        public void Smoothing_XAxis_Works()
        {
            var s = new ScrollSmoother();
            s.AddInput(50f, 0f);
            var total = 0;
            for (var i = 0; i < 200 && s.IsActive; i++)
            {
                s.Tick(Dt60, Tau, out var dx, out _);
                total += dx;
            }
            Assert.AreEqual(50, total);
        }

        // ---- Reset: 残距離が破棄される ----

        [Test]
        public void Reset_DiscardsRemainder()
        {
            var s = new ScrollSmoother();
            s.AddInput(0f, 500f);
            s.Reset();
            Assert.IsFalse(s.IsActive);
            s.Tick(Dt60, Tau, out _, out var dy);
            Assert.AreEqual(0, dy);
        }
    }
}
```

- [ ] **Step 3: コンパイルしてテストが失敗する (クラス未定義) ことを確認**

```bash
uloop compile
```

期待: `ErrorCount > 0`、エラーに `ScrollSmoother` が見つからない旨 (CS0246) が含まれる

- [ ] **Step 4: ScrollSmoother を実装する**

`cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollSmoother.cs` を作成:

```csharp
using System;

namespace CefUnity.Runtime
{
    /// <summary>
    ///     スクロール入力の平滑器 (指数追従)。生の wheel delta を「未送信の残距離」に
    ///     蓄積し、毎フレーム残距離の一定割合 (k = 1 - exp(-dt/τ)) を int px で排出する。
    ///     per-frame のスクロール送信量が均一化され、トラックパッド生 delta の
    ///     ジッター/フリック巨大単発 (診断: CoV 0.82, 最大 1138px/frame) が
    ///     幾何減衰のグライドに変わる。Chrome の入力リサンプリング (層2) 相当の
    ///     クライアント側実装。Unity API 非依存 (EditMode テスト可能)。
    ///     設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
    /// </summary>
    public sealed class ScrollSmoother
    {
        private float _remainX;
        private float _remainY;

        /// <summary>残距離が残っているか (排出継続の判定用)。</summary>
        public bool IsActive => _remainX != 0f || _remainY != 0f;

        /// <summary>入力 delta (px) を残距離に加算する。方向反転は符号の相殺で自然に処理。</summary>
        public void AddInput(float dxPx, float dyPx)
        {
            _remainX += dxPx;
            _remainY += dyPx;
        }

        /// <summary>残距離を破棄する (ナビゲーション時など)。</summary>
        public void Reset()
        {
            _remainX = 0f;
            _remainY = 0f;
        }

        /// <summary>
        ///     dt 秒経過分の排出量を計算する。tau &lt;= 0 は平滑 OFF
        ///     (従来挙動: int 切り捨て + 端数繰り越しで即時全量排出)。
        /// </summary>
        public void Tick(float dt, float tau, out int dx, out int dy)
        {
            // k < 0 を「平滑 OFF」の番兵に使う (排出率としての k は常に [0,1))。
            var k = tau <= 0f ? -1f : 1f - (float)Math.Exp(-dt / tau);
            dx = TickAxis(ref _remainX, k);
            dy = TickAxis(ref _remainY, k);
        }

        private static int TickAxis(ref float remain, float k)
        {
            if (remain == 0f) return 0;
            int emit;
            if (k < 0f)
            {
                // 平滑 OFF: 旧 _wheelAccum と同じ「切り捨て + 端数繰り越し」。
                emit = (int)remain;
                remain -= emit;
                return emit;
            }
            if (Math.Abs(remain) <= 1f)
            {
                // 終端スナップ: 無限テール防止。0.5px 未満の端数は破棄 (許容損失)。
                emit = (int)Math.Round(remain);
                remain = 0f;
                return emit;
            }
            emit = (int)Math.Round(remain * k);
            remain -= emit; // int で減算するので端数は残距離に残る (総量保存)
            return emit;
        }
    }
}
```

- [ ] **Step 5: コンパイル成功を確認**

```bash
uloop compile
```

期待: `Success: true, ErrorCount: 0`

- [ ] **Step 6: テストを実行して全パスを確認**

```bash
uloop run-tests --test-mode EditMode
```

期待: ScrollSmootherTests の 8 件を含む全 EditMode テストが PASS

- [ ] **Step 7: コミット (.meta を含める)**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollSmoother.cs \
        cef-unity-unityproject/Assets/CefUnity/Runtime/ScrollSmoother.cs.meta \
        cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollSmootherTests.cs \
        cef-unity-unityproject/Assets/CefUnity/Runtime.Tests/ScrollSmootherTests.cs.meta
git commit -m "feat: ScrollSmoother (スクロール入力の指数追従平滑器) + ユニットテスト"
```

---

### Task 2: CefUnityBrowserSample への統合

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs`

**Interfaces:**
- Consumes: Task 1 の `ScrollSmoother` (`AddInput` / `Tick` / `Reset` / `IsActive`)
- Produces: なし (最終統合)

- [ ] **Step 1: フィールドを置換する**

`_wheelAccumX, _wheelAccumY` フィールド宣言 (「int 切り捨てで捨てられる端数の繰り越し…」コメント含む、`WheelPixelsPerStep` 定義の直後) を以下に置換:

```csharp
        // スクロール平滑 (指数追従): 生 delta を残距離に蓄積し、毎フレーム均一化して排出。
        // 旧 _wheelAccum の端数繰り越し (トラックパッド慣性減衰の 0.0x 級微小 delta 対策)
        // は ScrollSmoother 内部に統合。設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
        private readonly ScrollSmoother _scrollSmoother = new ScrollSmoother();
        // 時定数 (秒)。$TMPDIR/cef_scroll_tau (ms 値のテキスト) で実行時上書き可 (0 = 平滑 OFF)。
        // 体感チューニング用の暫定機構 — 値確定後に const 化して撤去する。
        private float _scrollSmoothTau = 0.045f;
        private int _scrollTauCheckCountdown;
```

- [ ] **Step 2: HandleMouseInput の wheel 送信を蓄積に置換する**

`HandleMouseInput()` 末尾の以下のブロック:

```csharp
            var scroll = Input.mouseScrollDelta;
            if (scroll.y != 0f || scroll.x != 0f)
            {
                // ステップ→ピクセル変換。_resolutionScale で view (CSS px) が広がった分も
                // 掛けて、画面上の体感スクロール速度を scale に依らず一定に保つ
                // (マウス座標は既に scale 込みの view 座標へ変換している)。
                _wheelAccumX += scroll.x * WheelPixelsPerStep * _resolutionScale;
                _wheelAccumY += scroll.y * WheelPixelsPerStep * _resolutionScale;
            }
            var wheelDx = (int)_wheelAccumX;
            var wheelDy = (int)_wheelAccumY;
            if (wheelDx != 0 || wheelDy != 0)
            {
                _wheelAccumX -= wheelDx;
                _wheelAccumY -= wheelDy;
                _browser.SendMouseWheel(bx, by, wheelDx, wheelDy, mods);
                _inputSentThisFrame = true;
            }
            if (scroll.y != 0f) _frameSentDy = wheelDy; // 分析用: トラックパッドの送信量
```

を以下に置換 (送信は `TickScrollSmoother` に移管):

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

- [ ] **Step 3: 排出メソッドを追加する**

`HandleMouseInput()` メソッドの直後に追加:

```csharp
        /// <summary>
        /// ScrollSmoother の 1 フレーム分排出。蓄積された wheel 残距離を指数追従で
        /// 均一化して SendMouseWheel する (per-frame スクロール量ジッターの平滑)。
        /// HandleMouseInput の外に置くのは、カーソルがブラウザ外に出ても
        /// (TryGetBrowserCoord 失敗でも) グライド途中の排出を最後の有効座標で
        /// 継続するため。
        /// </summary>
        private void TickScrollSmoother()
        {
            // τ の実行時上書き (体感チューニング用・暫定)。毎フレーム I/O を避け 60F に 1 回。
            if (--_scrollTauCheckCountdown <= 0)
            {
                _scrollTauCheckCountdown = 60;
                var tauFile = System.IO.Path.Combine(System.IO.Path.GetTempPath(), "cef_scroll_tau");
                if (System.IO.File.Exists(tauFile)
                    && float.TryParse(System.IO.File.ReadAllText(tauFile).Trim(), out var ms))
                    _scrollSmoothTau = ms / 1000f;
            }
            if (!_scrollSmoother.IsActive) return;
            _scrollSmoother.Tick(Time.unscaledDeltaTime, _scrollSmoothTau, out var dx, out var dy);
            if (dx == 0 && dy == 0) return;
            // 最後の有効マウス座標で送る。まだ一度も動いていなければ画面中央。
            var bx = _lastMouseX >= 0 ? _lastMouseX : _currentWidth / 2;
            var by = _lastMouseY >= 0 ? _lastMouseY : _currentHeight / 2;
            _browser.SendMouseWheel(bx, by, dx, dy, GetCefModifiers());
            _inputSentThisFrame = true;
            _frameSentDy = dy; // 分析用: 平滑後の実送信量
        }
```

- [ ] **Step 4: OnEarlyUpdateLast に排出ステップを挿入する**

`OnEarlyUpdateLast()` 内、`cef_scroll_slow` 注入ブロックの閉じ括弧の直後・`self.UpdateCompositionCursorPos();` の直前に挿入:

```csharp
            // スクロール平滑の排出。BeginFrame#1 の前なので同フレームの paint に乗る。
            self.TickScrollSmoother();
```

- [ ] **Step 5: LoadUrl に Reset を追加する**

```csharp
        public void LoadUrl(string url)
        {
            _browser.LoadUrl(url);
        }
```

を以下に置換:

```csharp
        public void LoadUrl(string url)
        {
            // グライド途中の残距離を新ページへ流し込まない。
            _scrollSmoother.Reset();
            _browser.LoadUrl(url);
        }
```

- [ ] **Step 6: コンパイル成功と既存テスト全パスを確認**

```bash
uloop compile
uloop run-tests --test-mode EditMode
```

期待: `ErrorCount: 0`、既存テスト (CefAudioRingTests) 含め全 PASS

- [ ] **Step 7: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs
git commit -m "feat: wheel 入力を ScrollSmoother 経由の均一排出に切替 (スクロールカクツキ修正)"
```

注意: 同ファイルには既存の一時計装 (probe CSV 等) が未コミットで残っている。`git add -p` は使わず、このタスクの変更が計装と混在しても一括コミットで良い (計装は後続の撤去作業で削除される)。ただしコミット前に `git diff --staged` で意図しない削除がないか確認すること。

---

### Task 3: ビルド実測検証 (要ユーザー協力)

**Files:** 変更なし (検証のみ)

**Interfaces:**
- Consumes: Task 2 の統合済みビルド、既存の計測基盤 (`cef_perf_probe` → `$TMPDIR/cef_perf.csv`、`cef_load_url`、`/tmp/cef_heavy.html`)

- [ ] **Step 1: Editor を終了し batchmode でビルドする**

```bash
pkill -x Unity; sleep 3
UNITY_BIN=$(ls -d /Applications/Unity/Hub/Editor/*/Unity.app/Contents/MacOS/Unity | head -1)
"$UNITY_BIN" -batchmode -quit \
  -projectPath /Users/juha/Documents/GitHub/cef-unity/cef-unity-unityproject \
  -executeMethod CefUnity.Editor.CefQuickBuild.BuildMac \
  -logFile /tmp/cef_build.log
grep -E "result=|error" /tmp/cef_build.log | head -5
```

期待: `result=Succeeded`

- [ ] **Step 2: 計測フラグを設定してビルドを起動する**

```bash
touch "$TMPDIR/cef_perf_probe"
rm -f "$TMPDIR/cef_scroll_test" "$TMPDIR/cef_scroll_slow" "$TMPDIR/cef_scroll_tau" "$TMPDIR/cef_perf.csv"
echo "file:///tmp/cef_heavy.html" > "$TMPDIR/cef_load_url"
open /Users/juha/Documents/GitHub/cef-unity/build-mac/CefUnity.app
```

期待: ビルドが起動し、約15秒で重いページ (500カード) が表示される

- [ ] **Step 3: ユーザーに手動スクロールを依頼する**

ユーザーへの依頼内容: 「CefUnity ウィンドウをクリックしてアクティブにし、トラックパッドで穏やかなスクロールとフリックを 20 秒ほど行い、体感 (Arc との比較) を教えてください」

- [ ] **Step 4: probe CSV で平滑効果を定量確認する**

```bash
python3 - <<'PY'
import os, statistics as st
p = os.path.join(os.environ['TMPDIR'], 'cef_perf.csv')
rows = [l.split(',') for l in open(p) if l.strip()]
d = [(int(r[0]), float(r[1]), int(r[2]), int(r[3])) for r in rows if len(r) == 4]
dys = [abs(x[3]) for x in d if x[3] != 0]
gentle = [x for x in dys if x <= 120]
m, s = st.mean(gentle), st.pstdev(gentle)
print(f"スクロールフレーム={len(dys)} 穏やか={len(gentle)}")
print(f"per-frame |dy|: mean={m:.1f} std={s:.1f} CoV={s/m:.2f} (修正前 0.82 / 目標 <0.3)")
big = sum(1 for x in dys if x > 100)
print(f"|dy|>100px: {100*big/len(dys):.0f}% (修正前 28%)")
PY
```

期待: CoV < 0.3、巨大単発フレームの大幅減少

- [ ] **Step 5: τ の体感 A/B (ユーザー判断)**

```bash
echo 0 > "$TMPDIR/cef_scroll_tau"   # OFF (従来のカクつき確認)
echo 25 > "$TMPDIR/cef_scroll_tau"  # 弱め
echo 45 > "$TMPDIR/cef_scroll_tau"  # 既定
echo 80 > "$TMPDIR/cef_scroll_tau"  # 強め
```

各値でユーザーにスクロールしてもらい (反映まで最大60フレーム≈1秒)、最良の τ を決定する。決定値は Task 4 で const に固定する。

- [ ] **Step 6: 検証結果を記録する**

CoV 実測値・ユーザーの体感・決定 τ をプロジェクトメモリ `scroll-stutter-diagnosis.md` に追記する (コミット不要、メモリファイルは repo 外)。

---

### Task 4: τ 確定とチューニング機構の撤去 (Task 3 の体感確認後)

**Files:**
- Modify: `cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs`

**Interfaces:**
- Consumes: Task 3 で決定した τ 値 (以下、仮に 45ms として記載。**決定値で読み替えること**)

- [ ] **Step 1: τ を const に固定し、ファイル上書き機構を削除する**

Task 2 Step 1 で追加したフィールドを以下に置換 (`0.045f` は決定値に読み替え):

```csharp
        // スクロール平滑 (指数追従): 生 delta を残距離に蓄積し、毎フレーム均一化して排出。
        // 旧 _wheelAccum の端数繰り越し (トラックパッド慣性減衰の 0.0x 級微小 delta 対策)
        // は ScrollSmoother 内部に統合。設計: docs/superpowers/specs/2026-07-20-scroll-smoothing-design.md
        private readonly ScrollSmoother _scrollSmoother = new ScrollSmoother();
        // 時定数 (秒)。体感チューニング (2026-07-20) で確定した値。
        private const float ScrollSmoothTau = 0.045f;
```

`TickScrollSmoother()` 冒頭の τ ファイル読込ブロック (`if (--_scrollTauCheckCountdown <= 0) { ... }`) を削除し、`Tick` 呼び出しの `_scrollSmoothTau` を `ScrollSmoothTau` に変更する。`_scrollTauCheckCountdown` フィールドも削除する。

- [ ] **Step 2: コンパイルとテスト**

```bash
uloop compile
uloop run-tests --test-mode EditMode
```

期待: `ErrorCount: 0`、全 PASS

- [ ] **Step 3: コミット**

```bash
cd /Users/juha/Documents/GitHub/cef-unity
git add cef-unity-unityproject/Assets/CefUnity/Runtime/CefUnityBrowserSample.cs
git commit -m "chore: スクロール平滑の τ を確定値で固定しチューニング機構を撤去"
```
