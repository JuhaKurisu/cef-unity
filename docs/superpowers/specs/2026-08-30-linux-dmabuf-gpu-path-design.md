# Linux GPU ゼロコピー経路 (dmabuf) 設計書

作成日: 2026-08-30

前提: `docs/LINUX_GPU_FEASIBILITY.md` (CEF が dmabuf を供給できることの実証)、
`2026-07-29-linux-support-phase1-design.md` (Rust 単体 + Harness の Linux 動作)、
`2026-07-29-linux-phase2-deploy-and-ci-design.md` (Unity 配置と CI)。

## 到達ライン

**非 Unity ホスト (`CefUnity.Viewer`) で、CEF の描画が dmabuf 経由で GPU テクスチャとして
表示されるところまで。** Unity への組み込みは次フェーズに分離する。

macOS の IOSurface、Windows の D3D11/D3D12 共有テクスチャに相当する第 3 の経路を作る。

## 前提調査の結果

| 確認項目 | 結果 |
|---|---|
| CEF が dmabuf を供給するか | **する。** `ozone=x11` + `--enable-features=Vulkan` で 60 paints/秒 (`docs/LINUX_GPU_FEASIBILITY.md`) |
| CEF が渡す dmabuf の形 | `plane_count=1` / `modifier=0x0` (リニア) / `format=RGBA_8888` / fd 付き |
| NVIDIA の EGL/GL で dmabuf にレンダリングできるか | **できる。** GBM で確保 → `eglCreateImageKHR` → `glEGLImageTargetTexture2DOES` → FBO が COMPLETE になり、書き込んだ色が読み戻せることをスパイクで実測 |
| NVIDIA の GBM でリニア modifier を確保できるか | **できない** (`GBM_BO_USE_LINEAR` で `gbm_bo_create` が NULL)。ただし出力バッファは NVIDIA 固有 modifier で構わないため設計に影響しない |
| ヘッドレスで GPU 経路が動くか | **動かない。** GPU プロセスが SIGSEGV する。CI とコンテナは software paint のまま |
| `cc` クレートで C を足せるか | 既に `iosurface_pool.m` などで使用中。build-dependencies に入っている |
| Viewer の表示層 | `IFrameRenderer` / `FrameRendererFactory` で差し替え可能。Metal と D3D11 の 2 実装がある |

**当初 Vulkan が必須と推定していたが、スパイクで否定された。** Chromium 内部で観測した
`GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` は ANGLE 固有の事情であり、NVIDIA の GL 全般の
制限ではない。したがってサーバ側に Vulkan 依存 (`ash` 等) を持ち込まない。

## アーキテクチャ判断

### 既存 2 経路の構造をそのまま踏襲する

CEF が渡すテクスチャは **プールの借用** で、コールバックを抜けると再利用される
(`d3d11_pool.rs` の冒頭コメントに Windows 版の記述がある)。したがって macOS / Windows と同じく、

1. サーバ側に独自のグラフィックスデバイスを持つ
2. CEF のバッファを取り込む
3. 自前の出力バッファへ blit する
4. 出力バッファのハンドルをクライアントへ渡す

という流れになる。**CEF の fd を直接クライアントへ転送する案は採らない。** 次フレームで
上書きされ、macOS で実証済みのティアリング問題の Linux 版になる。

### fd は毎フレームではなく再作成時のみ送る

出力バッファはサイズ変更時のみ再作成する単一インスタンス構成 (`d3d11_pool.rs` と同じ)。
fd の転送は起動時とリサイズ時だけで、定常状態では 1 本も送らない。毎フレーム送ると
fd リークの温床になる。

### フレーム通知は既存の共有メモリヘッダを流用する

`SharedMemoryHeader` に `frame_id` があるので、ピクセルデータの代わりに世代情報を載せる。
クライアントのポーリング側に手を入れずに済む。

### サーバ側の pool は C で書く

EGL / GBM / GLES を叩く層。`iosurface_pool.m` (macOS) と同じ位置づけで、`cc` クレートで
ビルドする。Rust から EGL を叩くための新規クレート依存を増やさない。

## 変更点

### 1. `crates/server/src/dmabuf_pool.c` (新規)

EGL/GBM の初期化、CEF dmabuf の取り込み、出力バッファへの blit を担う。

公開する C API (Rust から呼ぶ):

| 関数 | 役割 |
|---|---|
| `dmabuf_pool_create()` | `/dev/dri/renderD128` を開き、GBM デバイスと EGL コンテキストを作る。<br>失敗時は NULL を返す (呼び出し側は software paint に落ちる) |
| `dmabuf_pool_destroy()` | 全リソースを解放する |
| `dmabuf_pool_blit(pool, cef_fd, cef_stride, cef_modifier, cef_fourcc, width, height)` | CEF の dmabuf を EGLImage 経由で取り込み、出力バッファへ描画して `glFinish` する。<br>サイズが変われば出力バッファを再作成し、`generation` を進める |
| `dmabuf_pool_output(pool, *fd, *stride, *modifier, *fourcc, *generation)` | 出力バッファの現在の記述子を返す。fd は dup 済みで、呼び出し側が閉じる |

出力バッファは `gbm_bo_create(..., GBM_BO_USE_RENDERING)` で確保する。modifier は
NVIDIA が選んだ値をそのまま使う (リニアを要求すると確保に失敗する)。

**スレッド親和性:** EGL コンテキストは作成したスレッドでしか current にできない。
`on_accelerated_paint` が来るスレッドに固定し、呼び出しスレッド ID が変化したら
WARNING をログに出す (`d3d11.rs` の既存の仕組みと同じ流儀)。

### 2. `crates/ipc/src/file_descriptor_channel.rs` (新規)

`SCM_RIGHTS` で fd を運ぶ Unix ドメインソケットのラッパー。Linux 専用。
`ipc-channel` の高レベル API は任意の fd を運べないため別建てにする。

- `FileDescriptorChannel::pair()` — テスト用の接続済み 1 対
- `listen(path)` / `connect(path)` — 実運用の経路
- `send(payload, file_descriptors)` / `receive(payload_buffer)`

ソケットのパスは `/tmp/cef_unity_dmabuf_<server_pid>_<browser_id>.sock`
(`shared_memory_flink_path` と同じ命名規則)。パスは既存の `Bootstrap` に載せて伝える。

送るペイロードは出力バッファの記述子: 幅・高さ・stride・modifier・fourcc・generation。

`libc` を Linux 限定の依存として `crates/ipc/Cargo.toml` に足す。

### 2-b. リサイズ時の世代整合

出力バッファを作り直すと fd が変わるため、クライアントが古いテクスチャを表示している
最中に新しい fd が届く。世代番号で解く。

- fd メッセージと共有メモリヘッダの双方に `generation` を持たせる
- **クライアントはヘッダの `generation` に一致するテクスチャだけを使う。**
  対応する fd を未受信なら、そのフレームは捨てて前フレームの絵を維持する

これにより「新しいサイズのヘッダを見たのに古いテクスチャを描く」という不整合が
構造的に起きなくなる。

なお CEF 側は `was_resized()` だけでは再描画されず `invalidate(VIEW)` が要る
(リポジトリルートの `CLAUDE.md` に既知の罠として記載がある)。ここも踏襲する。

### 3. `crates/server/src/server.rs` (変更)

- `linux_accelerated_paint_available` の判定に **「dmabuf プールの構築に成功したか」を足す**。
  Windows の `if d3d11_pool.is_some()` と同じ形。環境が足りなければ自動的に software paint に落ちる
- `on_accelerated_paint` の Linux 分岐を、破棄する実装から `dmabuf_pool_blit` の呼び出しに置き換える
- blit 後に `frame_id` と `generation` を共有メモリヘッダへ書く
- 出力バッファが再作成されたら、新しい fd を fd チャネルへ送る

### 4. `crates/client/src/dmabuf.rs` (新規)

受け取った fd を GL テクスチャにする。`d3d11.rs` / `metal_texture.m` に相当。

- fd チャネルへ接続し、記述子を受け取る
- `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` → `glEGLImageTargetTexture2DOES` で GL テクスチャ化
- `generation` ごとにキャッシュし、古いテクスチャと fd を解放する

**ホストの GL コンテキストが current なスレッドで呼ぶ必要がある。** `d3d11.rs` と同じく
呼び出しスレッド ID を記録し、変化したら WARNING を出す。

### 5. `CefUnity.Viewer/OpenGLFrameRenderer.cs` (新規) と `FrameRendererFactory` (変更)

`FrameRendererKind` に `OpenGL` を足し、`SelectKind(isMacOS, isWindows, isLinux)` を拡張する。
`D3D11FrameRenderer.cs` と同じ構造で、GL テクスチャを既定フレームバッファへ blit する。

### 6. `CefUnity.Harness` の `lifecycle` コマンド (変更)

macOS が `mach_ports` を数えている位置に、Linux では `/proc/self/fd` のエントリ数を出す。
`lifecycle 5` で横ばいなら fd リークなし、と機械的に判定できるようにする。

## 検証手順

1. `cargo test --workspace --lib --bins` — fd チャネルと判定ロジックのユニットテストが通る
2. `crates/server/tests/` の統合テストで、GPU 経路でフレームを取得して PNG に書き出し、
   内容を検証する (フェーズ 1 の合否判定と同じやり方)
3. `CefUnity.Viewer` を Linux 機で起動し、ブラウザが GPU 経路で表示されることを目視確認する
4. `lifecycle 5` で fd 数が横ばいであることを確認する
5. ウィンドウをリサイズし、絵が壊れないことを確認する
6. `DISPLAY` を外して起動し、software paint に落ちることを確認する (フォールバックの回帰)

3 番が本フェーズの合否判定。

## 未知のリスク

- **CEF のリニア dmabuf を取り込めるか。** スパイクで検証したのは NVIDIA 固有 modifier の
  バッファであり、CEF が渡す `modifier=0x0` の取り込みは未検証。取り込みは読み取り専用で
  書き込みより制約が緩いため通る見込みだが、実データで確認するまで確定しない
- **CEF の dmabuf の有効期間。** コールバックを抜けた後も fd が有効なのは確かだが (fd は
  参照カウントされたカーネルオブジェクト)、内容がいつ上書きされるかは CEF/Chromium の
  プール実装次第。blit をコールバック内で完結させる設計にしているため問題にならない想定
- **`glFinish` のコスト。** 60fps を維持できるかは実測が要る。macOS の `waitUntilCompleted`
  と同じ位置づけで、まず確実な形で動かしてから最適化を検討する

## スコープ外

- **Unity への組み込み** (次フェーズ)。Unity の Linux 既定は Vulkan なので、
  `EGL_EXT_image_dma_buf_import` ではなく Vulkan 外部メモリ経由の取り込みが要る可能性がある
- ヘッドレス環境での GPU 経路 (Chromium 側の制約で不可能)
- NVIDIA 以外の GPU (AMD / Intel) での検証。実機が無い
- Linux arm64
- ネイティブ音声出力

## 開発環境

ベアメタル Ubuntu 26.04 (`ssh ubuntu`、Tailscale `juha-ubuntu`)。RTX 3060 Ti /
`nvidia-driver-595-open`。編集と git は Mac 側、ビルドとテストは Linux 側という分担で、
同期は `~/.local/bin/cef-sync ubuntu` (rsync)。

GPU 経路の実行には X ディスプレイが要る。SSH からは
`DISPLAY=:0 XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1)` を指定する。
