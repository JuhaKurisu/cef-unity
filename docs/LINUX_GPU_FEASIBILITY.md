# Linux GPU ゼロコピー経路の実現可能性 — WSL2 での判定

計測日: 2026-08-29
環境: Windows デスクトップ (`juha-windows-desktop`) 上の WSL2 / Ubuntu 24.04 / x86_64 /
GeForce RTX 3060 Ti (NVIDIA WSL ドライバ 590.57) / WSL 2.7.11.0 / WSLg 1.0.73.2 /
カーネル 6.18.33.2-microsoft-standard-WSL2

## 結論

**WSL2 では Linux の GPU ゼロコピー経路 (dmabuf/EGL) を開発・検証できない。**
CEF の GPU プロセスが起動できないため、`on_accelerated_paint` は一度も呼ばれない。

原因は **DRM レンダーノード (`/dev/dri/renderD128`) が存在しないこと**。WSL の GPU 仮想化は
`/dev/dxg` + Mesa の d3d12 ドライバという独自経路で、標準の DRM デバイスを生やさない。
Chromium が dmabuf を確保するのに使う GBM はこのデバイスを要求するため、設定やスイッチでは
回避できない。

これは WSL の GPU 仮想化の構造に由来するもので、本リポジトリ側の不足ではない。

## 判定に使った実験

`experiment/linux-gpu-probe` ブランチ (恒久化しない):

1. `server.rs` — Linux でも `window_info.shared_texture_enabled = 1` を立てる
2. `server.rs` — `on_accelerated_paint` の Linux 分岐で `plane_count` / `modifier` / dmabuf fd を記録
3. `server.rs` — `--ozone-platform` を環境変数 `CEF_UNITY_OZONE` で差し替え可能にする
4. `Program.cs` — Harness を `CEF_UNITY_USE_GPU=1` / `CEF_UNITY_LOG=1` で GPU モード起動できるようにする

> Harness は既定で `CefRuntime.Initialize(useGpu: false)` を呼ぶ。ここを変えずに実験すると
> `use_gpu` が false のまま `--disable-gpu` が付き、GPU 経路の判定にならない。

## 結果

| ozone バックエンド | 結果 |
|---|---|
| `headless` (現行の既定) | GPU プロセスが SIGSEGV。`GPU process exited unexpectedly: exit_code=139` → `FATAL: GPU process isn't usable. Goodbye.` |
| `x11` (WSLg の `DISPLAY=:0`) | `ERROR:ui/gfx/linux/gbm_support_x11.cc:133] Can't create buffer -- gbm device is missing.` |
| `wayland` | CEF 初期化自体が失敗 (`ContentMainRun failed with exit code 1` → `CEF initialization failed (code -6)`) |

いずれのバックエンドでも `LINUX_ACCEL_PAINT` のログは 1 行も出力されず、
**`on_accelerated_paint` は一度も呼ばれなかった**。software paint (`on_paint`) は
全ケースで正常に動作し、`SMOKE_OK frames=2` を返している。

`x11` の `gbm device is missing` が原因を直接名指ししている。

## 裏付けとなる環境側の実測

```
$ ls /dev/dri
ls: cannot access '/dev/dri': No such file or directory     ← レンダーノードが無い

$ eglinfo
GBM platform:      eglInitialize failed                     ← GBM が初期化できない
Wayland platform:  eglInitialize failed
X11 platform:      EGL driver name: swrast                  ← EGL はソフトウェア
libEGL warning: DRI3 error: Could not get DRI3 device
```

CEF 145 の Linux ヘッダには accelerated paint の定義自体は存在する
(`cef_types_linux.h:181` の `_cef_accelerated_paint_info_t` — `planes` が dmabuf の fd、
`modifier` は EGL 用)。**CEF 側の API は揃っており、足りないのは環境だけ**である。

## 誤診しやすい点

**`GALLIUM_DRIVER=d3d12` を指定すると GLX は GPU で動く。**

```
$ DISPLAY=:0 GALLIUM_DRIVER=d3d12 glxinfo -B
Device: D3D12 (NVIDIA GeForce RTX 3060 Ti)
Accelerated: yes
```

指定しないと `llvmpipe` (CPU) に落ちる。この結果だけを見て「GPU が使える環境だ」と
判断すると誤る。**GLX が動くこと と Chromium が dmabuf を確保できること は別問題**で、
後者には GBM が要る。

## WSL2 で進められる作業 / 進められない作業

| 作業 | 可否 |
|---|---|
| Rust / C# のビルド、ユニットテスト | 可 |
| software paint 経路の動作確認 (Harness smoke / dump) | 可 |
| ヘッドレス環境の検証 | 可 |
| ネイティブスクロール入力の Linux 実装 (XInput2) | 可 (X11 は WSLg にある) |
| **GPU ゼロコピー (dmabuf/EGL)** | **不可** |
| Unity Editor for Linux の実機確認 | 要検証 (Editor 自体が GPU を要求するため、software GL で実用になるか未確認) |

## GPU 経路をやる場合の選択肢

1. **同じデスクトップに Linux をベアメタルで入れる** — RTX 3060 Ti がそのまま使え、
   `/dev/dri` も本来のドライバも揃う。最も確実
2. **GPU 付きのクラウド Linux VM** — 実 x86_64 + 実レンダーノード。時間課金

なお `dmesg` に `dxgkio_query_adapter_info: Ioctl failed: -22 / -2`、
`dxgvmb_send_create_sync_object: failed -75` が出ており、WSL の GPU スタックが
完全に健全とは言えない。WSL や GPU ドライバの更新で `/dev/dri` が生える可能性は
否定できないが、Windows 側の操作と WSL 再起動を伴うため未試行。
