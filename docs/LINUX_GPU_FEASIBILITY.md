# Linux GPU ゼロコピー経路の実現可能性 — 判定結果

計測日: 2026-08-29 (WSL2) / 2026-08-30 (ベアメタル)

## 結論

**ベアメタル Linux では GPU ゼロコピー (dmabuf) が動く。60 paints/秒が持続することを実測した。**
**WSL2 では動かない。**

動作した構成:

| 項目 | 値 |
|---|---|
| ozone プラットフォーム | **`x11`** (実 X ディスプレイが必要。Xwayland 可) |
| 追加スイッチ | **`--enable-features=Vulkan`** (Skia の Vulkan バックエンド) |
| `window_info.shared_texture_enabled` | `1` |

実測結果 (`paint-statistics 10 1280 720 animation`):

```
paints=60 copies=0    ← 1 秒ごと、10 秒間安定
LINUX_ACCEL_PAINT #1 plane_count=1 modifier=0x0 format=1 size=1280x720 fd0=149
```

dmabuf は 1 プレーン、modifier は `0x0` (DRM_FORMAT_MOD_LINEAR)。

## 検証環境

**ベアメタル**: Ubuntu 26.04.1 LTS / カーネル 7.0.0-30 / x86_64 /
GeForce RTX 3060 Ti / `nvidia-driver-595-open` 595.84 / `nvidia_drm.modeset=Y` /
GNOME Wayland セッション (Xwayland `:0` あり)

**WSL2** (比較対象): Ubuntu 24.04 / WSL 2.7.11.0 / WSLg 1.0.73.2 / 同じ GPU

## 組み合わせごとの結果 (ベアメタル)

| ozone | Skia バックエンド | 結果 |
|---|---|---|
| **`x11`** | **Vulkan** | **成功。60 paints/秒** |
| `wayland` | Vulkan | accelerated paint は 1 回届くが Chromium が非互換を報告:<br>`'--ozone-platform=wayland' is not compatible with Vulkan` |
| `wayland` | 既定 (ANGLE GL) | `GL framebuffer returned incomplete: 0x8CD6` → `Unable to initialize SkSurface`。paint ゼロ |
| `headless` | Vulkan / 既定 / どれも | GPU プロセスが SIGSEGV (`exit_code=139`) → `GPU process isn't usable` |

**ヘッドレスでは GPU 経路が成立しない。** OSR にウィンドウは不要だが、Chromium の GL/GPU
初期化には実ディスプレイが要る。CI ランナーやコンテナでは software paint 経路のままになる
(そちらは従来どおり正常に動く)。

## 効かなかったもの (切り分け済み)

以下はいずれも原因でも解決策でもなかった:

- `--enable-native-gpu-memory-buffers` — `SkSurface` 失敗は変わらない
- `--use-angle=vulkan` 単独 — ANGLE を Vulkan にしても Skia が GL のままだと同じ失敗
- `--in-process-gpu` — X ディスプレイを開けず失敗
- `nvidia_drm.modeset` — 既に `Y` (`fbdev` も `Y`)
- `/dev/dri`・EGL・GBM — すべて正常 (下記)

決め手は **Skia を Vulkan バックエンドにすること**だった。ANGLE の GL 経由で dmabuf を
インポートするとテクスチャがレンダリング可能にならず (`GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT`)、
SkSurface が作れない。Vulkan の外部メモリ経由なら NVIDIA でも通る。

## WSL2 が不可な理由

**DRM レンダーノード `/dev/dri` が存在しない。** WSL の GPU 仮想化は `/dev/dxg` + Mesa の
d3d12 ドライバという独自経路で、標準の DRM デバイスを生やさない。

```
WSL2:        ls /dev/dri  →  No such file or directory
             eglinfo GBM platform  →  eglInitialize failed
             eglinfo X11 platform  →  EGL driver name: swrast
             ozone=x11 →  Can't create buffer -- gbm device is missing.

ベアメタル:   /dev/dri/card1, /dev/dri/renderD128
             eglinfo GBM platform  →  EGL vendor string: NVIDIA
             EGL_EXT_image_dma_buf_import / _modifiers / EGL_MESA_image_dma_buf_export
```

> **訂正 (2026-08-30):** 当初この文書は WSL2 の GPU プロセス SIGSEGV を `/dev/dri` の欠如が
> 原因と記述していたが、**これは誤りだった**。同じ `exit_code=139` はベアメタルの
> `--ozone-platform=headless` でも再現する。あの SIGSEGV の主因は GL 初期化が X ディスプレイを
> 開けないこと (`ANGLE Display::initialize error 12289: Could not open the default X display`) で、
> `/dev/dri` の欠如は x11 経路で出た別の症状 (`gbm device is missing`)。
> **WSL2 が不可という結論自体は変わらない**が、理由の切り分けが不正確だった。

## 実装への影響

現状 `server.rs` は Linux で **無条件に `--ozone-platform=headless`** を付けている。
GPU 経路を有効にするには、ここを条件分岐させる必要がある。

- GPU モード かつ ディスプレイあり → `x11` + `--enable-features=Vulkan`
- それ以外 (ヘッドレス、CI、GPU 無効) → 現行どおり `headless` + software paint

`--ozone-platform=headless` が入った経緯は
`docs/superpowers/specs/2026-07-29-linux-phase2-deploy-and-ci-design.md` の
「ヘッドレス問題への対処」にある。CI を緑にするための対処なので、これを壊さないこと。

## 誤診しやすい点

**`GALLIUM_DRIVER=d3d12` を指定すると WSL2 でも GLX は GPU で動く。**

```
$ DISPLAY=:0 GALLIUM_DRIVER=d3d12 glxinfo -B
Device: D3D12 (NVIDIA GeForce RTX 3060 Ti)
Accelerated: yes
```

指定しないと `llvmpipe` (CPU) に落ちる。この結果だけを見て「GPU が使える環境だ」と
判断すると誤る。GLX が動くこと と Chromium が dmabuf を確保できること は別問題。

**Harness は既定で `CefRuntime.Initialize(useGpu: false)`。**
ここを変えずに GPU 実験をすると `--disable-gpu` が付いたままで判定にならない
(`paint-statistics` コマンドは `useGpu: true` なのでそのまま使える)。

## 判定に使った実験

`experiment/linux-gpu-probe` ブランチ (恒久化しない):

1. `server.rs` — Linux でも `window_info.shared_texture_enabled = 1` を立てる
2. `server.rs` — `on_accelerated_paint` の Linux 分岐で `plane_count` / `modifier` / dmabuf fd を記録
3. `server.rs` — `--ozone-platform` を環境変数 `CEF_UNITY_OZONE` で差し替え可能にする
4. `server.rs` — 任意の Chromium スイッチを `CEF_UNITY_EXTRA_SWITCHES` で差し込めるようにする
5. `Program.cs` — Harness の smoke を `CEF_UNITY_USE_GPU=1` / `CEF_UNITY_LOG=1` で GPU 起動できるようにする

再現手順:

```bash
export XDG_RUNTIME_DIR=/run/user/1000 DISPLAY=:0
export XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1)
CEF_UNITY_OZONE=x11 CEF_UNITY_EXTRA_SWITCHES="enable-features=Vulkan" \
  ./CefUnity.Harness paint-statistics 10 1280 720 animation
```

SSH から X を使うには `XAUTHORITY` の指定が要る (GNOME Wayland では
`/run/user/1000/.mutter-Xwaylandauth.XXXXXX`。ドット始まりなので `ls -a` で探すこと)。

## CEF 側の状況

CEF 145 の Linux ヘッダには accelerated paint の定義がある
(`cef_types_linux.h:181` の `_cef_accelerated_paint_info_t` — `planes` が dmabuf の fd、
`modifier` は EGL 用)。ただし cefclient に Linux 実装が無く、
[issue #3687](https://github.com/chromiumembedded/cef/issues/3687) は現在も open で
「未検証」の扱い。**本計測はその未検証部分が実際に動くことを確認したことになる。**
