# Linux GPU ゼロコピー経路の実現可能性 — 判定結果

計測日: 2026-08-29 (WSL2) / 2026-08-30 (ベアメタル)

## 結論

**未解決。** ベアメタル Linux で `on_accelerated_paint` は 60 回/秒 呼ばれるが、
**渡される dmabuf の中身は空**である (2026-08-30 に判明)。
**WSL2 ではそもそも GPU プロセスが起動しない。**

> **訂正 (2026-08-30):** この文書は当初「GPU ゼロコピーが動く。60 paints/秒が持続する」と
> 結論していたが、**誤りだった**。根拠にしていたのは `on_accelerated_paint` の呼び出し回数
> だけで、バッファの中身を一度も検証していなかった。後に dmabuf を CPU から mmap して全走査
> したところ、**非ゼロバイトが 1 つも無い**ことが分かった。
> **呼び出し回数は経路が動いていることの証拠にならない。**

`on_accelerated_paint` が呼ばれる構成 (中身は空):

| 項目 | 値 |
|---|---|
| ozone プラットフォーム | **`x11`** (実 X ディスプレイが必要。Xwayland 可) |
| 追加スイッチ | **`--enable-features=Vulkan`** (Skia の Vulkan バックエンド) |
| `window_info.shared_texture_enabled` | `1` |

実測結果:

```
paints=60 copies=0                              ← 呼び出しは 1 秒ごとに 60 回、安定
CPU 直読み: 中心 RGBA=(0,0,0,0) 非ゼロバイト数=0  ← しかしバッファは全ゼロ
CEF の矩形: visible=(0,0,1280x720) content=(0,0,1280x720) source=1280x720
```

dmabuf は 1 プレーン、modifier は `0x0` (DRM_FORMAT_MOD_LINEAR)、stride 5120 = 1280×4。
矩形はフルフレームなので「内容が別の位置にある」わけでもない。

## 中身が空である件の切り分け (2026-08-30)

以下はすべて計測で否定した。**取り込み側 (我々のコード) の問題ではない。**

| 疑った箇所 | 判定 |
|---|---|
| GL の取り込み (EGLImage) の不備 | **否定** — dmabuf を CPU から mmap しても全ゼロ |
| GPU の書き込み完了を待てていない | **否定** — 1 フレーム遅らせて読んでも全ゼロ |
| modifier の指定方法 | **否定** — 明示しても省略しても全ゼロ |
| `visible_rect` / `content_rect` のオフセット | **否定** — フルフレーム |
| 出力 FBO が共有バッファを指していない | **否定** — 赤でクリアすると読み戻せる |
| プールが別スレッドで二重初期化 | **否定** — 警告ゼロ |
| Chromium が GL コンテキストを奪う | **否定** — 退避・復帰を入れても不変 |
| Vulkan バックエンドの要否 | Vulkan 無しでは paint 自体が 0 件。**必須だが十分ではない** |

同じ機械で **software paint 経路は同じページを正しく描画する**ため、CEF・ドライバ・
ネットワークはいずれも正常である。

さらに Vulkan (`VK_EXT_image_drm_format_modifier` で import → `vkCmdCopyImageToBuffer` →
読取) でも全ゼロだった。**書いた本人 (Chromium は GaneshVulkan) と同じ API で読んでも
空**なので、取り込み方の問題ではなく「エクスポートされたメモリに一度も書かれていない」
ことが確定した。

追加で確定した事実:

- fd は本物の dmabuf (`readlink /proc/self/fd/N` → `/dmabuf:`、3.7 MB)
- CEF 145 (branch 7632) は viz に `kPreferGpuMemoryBuffer` を正しく渡している
- viz (`FrameSinkVideoCapturerImpl`) は `BlitRequest(populates_gpu_memory_buffer=true)`
  を発行し、フレームを「成功」として配送している — つまり失敗は無音
- 全 600 フレームをサンプルしても全ゼロ (初回フレームだけの問題ではない)
- **nvidia-driver-595-open と 610.43.02-open の両方で再現** (ドライバ更新では直らない)
- 試して効果が無かった Chromium スイッチ: `use-angle=vulkan` / `use-vulkan=native` /
  外部 BeginFrame 無効化。`SkiaGraphite` / `DefaultANGLEVulkan` / `in-process-gpu` は
  コールバック自体が止まる

CEF の [issue #3687](https://github.com/chromiumembedded/cef/issues/3687) が
「Linux の `OnAcceleratedPaint` は cefclient に実装が無く未検証」としているのと整合する。
**API は呼ばれるが中身が来ない**、というのが現時点の観測。上流への報告が妥当。

なお本リポジトリ側の実装 (fd 転送・blit・取り込み・Viewer 表示) は、既知の色を入れた
自前バッファでの単体テストと fd 転送の E2E で全て動作確認済みであり、ソースが空である
こと以外に問題は無い。

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
「未検証」の扱い。**本計測は、その未検証部分が「呼ばれはするが中身が来ない」状態にあることを示している。**
