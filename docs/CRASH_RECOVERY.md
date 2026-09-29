# クラッシュからの自動復旧

CEF のプロセスが落ちても、利用側 (Unity / Viewer) は何もしなくても元に戻る。
`Browser` のハンドルはそのまま使い続けてよい。

障害は 3 種類あり、それぞれ別の仕組みで戻す。

| 障害 | 検出 | 復旧 | ページの状態 |
|---|---|---|---|
| ① レンダラー (タブ) のクラッシュ・無応答 | CEF の `on_render_process_terminated` / `on_render_process_unresponsive` | server がそのブラウザを再読み込み (無応答なら強制終了してから) | 読み込み直し |
| ② GPU プロセスのクラッシュ | 合成経路の生存確認 (下記) | server が同じ server 内でブラウザを作り直す | 読み込み直し |
| ③ server プロセスの死亡・固まり | client の監視スレッド (プロセス終了・heartbeat 停止)、IPC のエラー・応答タイムアウト | client が server を再起動し、全ブラウザを作り直す | 読み込み直し。Cookie 等はディスクのキャッシュから戻る |

入力中のフォームや JavaScript の状態は、どの復旧でも失われる。

## 利用側の API

復旧は `CefRuntime.Pump()` (毎フレーム呼ぶもの) の中で進み、状態が変わったフレームでイベントが出る。

```csharp
CefRuntime.ServerLost += status => ...;           // ③ server を失った (復旧は自動)
CefRuntime.ServerRecovered += status => ...;      // ③ 再起動して全ブラウザを作り直した
CefRuntime.ServerRecoveryFailed += status => ...; // ③ 諦めた。Shutdown → Initialize でやり直す
browser.RenderProcessTerminated += (browser, status) => ...; // ① レンダラーが終了した
browser.Recreated += (browser, status) => ...;    // ②③ ブラウザを作り直した (JS の注入はここで入れ直す)

CefRuntime.GetServerStatus();   // 状態・最後に失った理由・回数
browser.GetRecoveryStatus();    // レンダラー終了回数・再読み込み停止中か・作り直し回数
```

`CefUnityBrowserSample` はこれらをログに出している。

## ① レンダラー

- 終了したら `browser.reload()` で読み込み直す (最後にコミットしたページが戻る)
- 無応答 (Chromium のハングモニタが入力の未応答を検出) は、OSR には「待つ/終了」の選択 UI が
  無いので強制終了し、同じ経路で戻す
- **60 秒に 3 回落ちたら再読み込みを止める** (ページ自体が確実に落とす場合のループ防止)。
  止めている間は空白で、`RenderProcessReloadSuppressed` が true になる。
  `LoadUrl` で別のページを開くと解除される

## ② GPU プロセス

GPU プロセスが落ちると、Chromium は GPU プロセスを起動し直す (約 50ms)。しかし**既存ブラウザの
OSR の合成経路は繋ぎ直されず、以後 paint が一切来ない**。CEF 145 (macOS arm64) での実測:

| 試したこと | paint |
|---|---|
| BeginFrame を送り続ける | 0 |
| `invalidate` / `was_resized` (Resize) | 0 |
| `was_hidden(1)` → `was_hidden(0)` → `invalidate` | 0 |
| 再読み込み (`LoadUrl` で同じ URL) | 0 |
| 外部 BeginFrame を使わない構成 (`external_begin_frame_enabled=0`) | 0 |
| **同じ server 内で新しくブラウザを作る** | **正常に描画** |

server の CEF ログでは BeginFrame は受け取り続けており (54/秒)、GPU プロセスも
"Reinitialized the GPU process after a crash" と再初期化されている。
GPU プロセスの起動失敗が続くと Chromium は `GPU process isn't usable. Goodbye.` で
server ごと FATAL 終了する。その場合は③で戻る。

CEF は GPU プロセスのクラッシュを知らせるコールバックを持たないので、症状から検出する
(`crates/server/src/compositor_probe.rs`):

1. 3 秒 paint が無いブラウザに `invalidate(VIEW)` を送る。健全なら次の BeginFrame で必ず paint が返る
2. 1 秒以内に返らなければ失敗。**2 回続けて失敗したら**合成経路が死んだとみなし、
   同じ server 内でブラウザを作り直す (共有メモリと browser_id はそのまま = client は何もしない)
3. 誤検出するとページが読み込み直しになるため、次の間は判定しない:
   一度も描画していない (初回読み込みが遅いだけ)・レンダラーが死んでいる・BeginFrame が
   0.5 秒以上来ていない (利用側が止まっている)
4. 作り直しは 5 分に 3 回まで (GPU が壊れ続けている場合のループ防止)

副作用として、**静止ページでは 3 秒ごとに 1 回 paint が出る** (確認の invalidate による)。
画面の内容は変わらない。

GPU プロセスの PID の変化を見る方法も検討したが、macOS の GPU プロセスは
`cef-unity-server Helper.app` (Renderer 等と同じ実行ファイル) から `--type=gpu-process` で
起動され、実行ファイル名では区別できない。3 OS それぞれで他プロセスの引数を読む実装が要るため、
OS 非依存の症状検出を選んだ。

## ③ server

### 検出 (client)

| 経路 | 内容 |
|---|---|
| 監視スレッド (`crates/client/src/watchdog.rs`) | 250ms ごとに `try_wait` でプロセス終了を見る。server status 共有メモリの heartbeat (イベントループの tick ごとに +1) が **40 回 (10 秒) 続けて進まなければ固まった**とみなして kill する |
| IPC | 送受信のエラー。応答を待つ呼び出しは **10 秒でタイムアウト**して kill する (以前は無期限で、固まった server への `GetLogs` 等で Unity のメインスレッドが永久に止まり得た) |

heartbeat は経過時間ではなく観測回数で数える。PC のスリープ復帰直後は経過時間が一気に伸びるが、
server もすぐ動き出すので観測回数なら誤検出しない。

固まったことの報告は kill より先に行う。kill した瞬間からメインスレッドの送信も失敗し始めるため、
後に回すと理由が「接続断」として記録される (実測)。逆に IPC エラーで見つかった場合は、
プロセスの終了状態が取れるまで最大 200ms 待って「終了」として報告する
(SIGKILL 直後は IPC のポートが先に壊れ、終了状態がわずかに遅れて取れる。実測)。

### 復旧 (client、`cef_unity_pump` 上)

1. 死んだ server を kill して回収する。Mach 受信ポートを破棄する
2. 専用スレッドで server を起動する (bootstrap まで最大 15 秒。メインスレッドは止めない)
3. 起動できたら、全ブラウザを同じハンドルのまま作り直す。開く URL は、死んだ server が
   共有メモリに記録していたメインフレームの URL (`on_address_change`。ページ内の JS による
   遷移も含む)。無ければ最後に `LoadUrl` した URL。サイズは最後に `Resize` した値。
   ネイティブ音声を使っていたら、新しい server の音声フォーマットが確定した時点で再開する
4. 死んだ server の共有メモリ (映像・音声・status) は server が後始末できないので、
   client が所有権を引き取って消す

- 起動の再試行: すぐ → 1 秒後 → 3 秒後。**2 回目以降はキャッシュ (`$TMPDIR/cef_unity_cache`) を
  消してから起動する** (強制終了の繰り返しでキャッシュが壊れ、起動がハングした実例があるため)
- **5 分に 4 回 server を失ったら諦める** (`ServerRecoveryFailed`)
- レンダラー終了回数と `accelerated_frame_id` は server ごとに 0 から数え直されるが、client が
  前の server の分を足して単調増加で見せる (0F 待ちは増分で到着を判定するため)

## 検証 (Harness)

`CefUnity.Harness crash-recovery <scenario> [gpu|cpu]` で実際にプロセスを落として確かめる。

| シナリオ | 内容 | 結果 (2026-09-29, macOS arm64) |
|---|---|---|
| `renderer` | `chrome://crash` → SIGSEGV を 2 回。2 回までは再読み込みで描画が戻り、3 回目で止まる。`LoadUrl` で戻る | GPU/CPU とも OK |
| `server-kill` | JS で別ページへ遷移してから server を SIGKILL。1 秒未満で再起動し、遷移先の URL で描画が戻る。死んだ server の共有メモリが残らない | GPU/CPU とも OK |
| `server-hang` | server を SIGSTOP。10.6 秒で `Unresponsive` として検出して復旧 | GPU/CPU とも OK |
| `gpu-crash` | 静止ページで 12 秒間作り直しが起きないこと。`chrome://gpucrash` を 2 回、それぞれ約 5 秒で作り直して描画が戻る | OK |

Unity Editor (6000.3.8f1) でも、Play 中に server を SIGKILL して自動で描画が戻ること、
Stop 後に server・共有メモリが残らないことを確認した。

## 既知の制約

- Windows・Linux は CI でのビルドのみで、クラッシュ試験は未実施 (Harness の `crash-recovery` は
  `pgrep` / `kill` を使うため macOS・Linux 用)
- server が固まった場合、検出までの最大 10 秒間は描画が止まる。その間に IPC の送信キューが
  溢れると、送信するメインスレッドもその間止まる
- 復旧中 (`Recovering`) に `new Browser(...)` すると失敗する (例外)。復旧後に作り直すこと
