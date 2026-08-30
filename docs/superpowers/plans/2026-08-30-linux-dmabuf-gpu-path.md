# Linux dmabuf GPU 経路 実装計画

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** CEF の `on_accelerated_paint` が渡す dmabuf を、サーバ側の出力バッファへ blit してクライアントへ fd で渡し、`CefUnity.Viewer` の画面に GPU テクスチャとして表示する。

**Architecture:** macOS の IOSurface、Windows の D3D11 共有テクスチャと同じ「CEF のバッファは借用なので自前の出力バッファへ blit し、そのハンドルをクライアントへ渡す」構造。サーバ側は EGL/GBM/GLES を C で叩き、fd は Unix ドメインソケットの `SCM_RIGHTS` で運ぶ。fd の転送は起動時とリサイズ時のみで、フレーム通知は既存の共有メモリヘッダを流用する。

**Tech Stack:** Rust (server / client / ipc クレート)、C (`cc` クレートでビルド)、EGL / GBM / OpenGL ES 2、C# (`CefUnity.Viewer`、Silk.NET)

**Spec:** `docs/superpowers/specs/2026-08-30-linux-dmabuf-gpu-path-design.md`

## Global Constraints

- **識別子は省略形を使わない** (リポジトリルート `CLAUDE.md`)。`fd` は例外として扱わず `file_descriptor` と書く。ただし C の構造体メンバや EGL/GBM の API 名など外部由来の名前は変更しない
- **Linux 固有コードは `#[cfg(target_os = "linux")]` の中に閉じる。** macOS / Windows のビルドを壊さない
- **既定の挙動を壊さない。** `DISPLAY` が無い環境、GPU プールの構築に失敗した環境では従来どおり software paint に落ちる
- **CI は `cargo test --workspace --lib --bins --release` を実行する。** テストはこれで拾われる場所に置く
- 開発環境: 編集と git は Mac、ビルドとテストは `ssh ubuntu`。同期は `~/.local/bin/cef-sync ubuntu`
- GPU 経路の実行には `DISPLAY=:0 XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1)` が要る
- 作業ブランチは `feat/linux-gpu-ozone-selection` (ozone 選択の実装が既に入っている)

---

### Task 1: fd 転送チャネル (SCM_RIGHTS)

**Files:**
- Create: `cef-unity-rust/crates/ipc/src/file_descriptor_channel.rs`
- Modify: `cef-unity-rust/crates/ipc/src/lib.rs` (モジュール宣言)
- Modify: `cef-unity-rust/crates/ipc/Cargo.toml` (Linux 限定で `libc`)

**Interfaces:**
- Consumes: なし
- Produces: `FileDescriptorChannel::pair() -> std::io::Result<(Self, Self)>`、
  `FileDescriptorChannel::send(&self, payload: &[u8], file_descriptors: &[RawFd]) -> std::io::Result<()>`、
  `FileDescriptorChannel::receive(&self, payload_buffer: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)>`

> 注: このファイルの雛形とテスト 1 件は既に作業ツリーに存在する (RED の状態)。既存の内容を出発点にしてよい。

- [ ] **Step 1: 失敗するテストを書く**

`cef-unity-rust/crates/ipc/src/file_descriptor_channel.rs` の `mod tests` に:

```rust
#[test]
fn 送った_fd_で受信側が同じファイルを読める() {
    let path = std::env::temp_dir().join("cef_unity_fd_channel_test.txt");
    std::fs::File::create(&path).unwrap().write_all(b"dmabuf").unwrap();
    let file = std::fs::File::open(&path).unwrap();

    let (sender, receiver) = FileDescriptorChannel::pair().unwrap();
    sender.send(b"x", &[std::os::fd::AsRawFd::as_raw_fd(&file)]).unwrap();

    let mut payload_buffer = [0u8; 8];
    let (_length, file_descriptors) = receiver.receive(&mut payload_buffer).unwrap();

    assert_eq!(file_descriptors.len(), 1);
    let mut received = String::new();
    std::fs::File::from(file_descriptors.into_iter().next().unwrap())
        .read_to_string(&mut received)
        .unwrap();
    assert_eq!(received, "dmabuf");
}
```

- [ ] **Step 2: 失敗を確認する**

```bash
~/.local/bin/cef-sync ubuntu
ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-ipc --lib file_descriptor'
```

Expected: FAIL — `not yet implemented` (`todo!()` で落ちる)

- [ ] **Step 3: 最小実装**

`sendmsg` / `recvmsg` の補助データに `SCM_RIGHTS` を載せる。

```rust
impl FileDescriptorChannel {
    pub fn pair() -> std::io::Result<(Self, Self)> {
        let (first, second) = std::os::unix::net::UnixStream::pair()?;
        Ok((Self { socket: first }, Self { socket: second }))
    }

    pub fn send(&self, payload: &[u8], file_descriptors: &[RawFd]) -> std::io::Result<()> {
        let mut control_buffer =
            vec![0u8; unsafe { libc::CMSG_SPACE((size_of::<RawFd>() * file_descriptors.len()) as u32) } as usize];
        let mut io_vector = libc::iovec {
            iov_base: payload.as_ptr() as *mut libc::c_void,
            iov_len: payload.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut io_vector;
        message.msg_iovlen = 1;
        if !file_descriptors.is_empty() {
            message.msg_control = control_buffer.as_mut_ptr() as *mut libc::c_void;
            message.msg_controllen = control_buffer.len();
            unsafe {
                let control_message = libc::CMSG_FIRSTHDR(&message);
                (*control_message).cmsg_level = libc::SOL_SOCKET;
                (*control_message).cmsg_type = libc::SCM_RIGHTS;
                (*control_message).cmsg_len =
                    libc::CMSG_LEN((size_of::<RawFd>() * file_descriptors.len()) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    file_descriptors.as_ptr(),
                    libc::CMSG_DATA(control_message) as *mut RawFd,
                    file_descriptors.len(),
                );
            }
        }
        let sent = unsafe {
            libc::sendmsg(std::os::fd::AsRawFd::as_raw_fd(&self.socket), &message, 0)
        };
        if sent < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn receive(&self, payload_buffer: &mut [u8]) -> std::io::Result<(usize, Vec<OwnedFd>)> {
        const MAXIMUM_FILE_DESCRIPTORS: usize = 4; // dmabuf のプレーン数上限に合わせる
        let mut control_buffer =
            vec![0u8; unsafe { libc::CMSG_SPACE((size_of::<RawFd>() * MAXIMUM_FILE_DESCRIPTORS) as u32) } as usize];
        let mut io_vector = libc::iovec {
            iov_base: payload_buffer.as_mut_ptr() as *mut libc::c_void,
            iov_len: payload_buffer.len(),
        };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut io_vector;
        message.msg_iovlen = 1;
        message.msg_control = control_buffer.as_mut_ptr() as *mut libc::c_void;
        message.msg_controllen = control_buffer.len();

        let received = unsafe {
            libc::recvmsg(std::os::fd::AsRawFd::as_raw_fd(&self.socket), &mut message, 0)
        };
        if received < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut file_descriptors = Vec::new();
        unsafe {
            let mut control_message = libc::CMSG_FIRSTHDR(&message);
            while !control_message.is_null() {
                if (*control_message).cmsg_level == libc::SOL_SOCKET
                    && (*control_message).cmsg_type == libc::SCM_RIGHTS
                {
                    let payload_length = (*control_message).cmsg_len
                        - libc::CMSG_LEN(0) as usize;
                    let count = payload_length / size_of::<RawFd>();
                    let data = libc::CMSG_DATA(control_message) as *const RawFd;
                    for index in 0..count {
                        file_descriptors
                            .push(std::os::fd::FromRawFd::from_raw_fd(*data.add(index)));
                    }
                }
                control_message = libc::CMSG_NXTHDR(&message, control_message);
            }
        }
        Ok((received as usize, file_descriptors))
    }
}
```

構造体のフィールド名は `socket` に変更する (雛形では `_socket`)。

- [ ] **Step 4: 通ることを確認する**

```bash
~/.local/bin/cef-sync ubuntu
ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-ipc --lib file_descriptor'
```

Expected: PASS

- [ ] **Step 5: ペイロードのテストを足して RED→GREEN を回す**

```rust
#[test]
fn ペイロードが往復する() {
    let (sender, receiver) = FileDescriptorChannel::pair().unwrap();
    sender.send(b"hello", &[]).unwrap();
    let mut payload_buffer = [0u8; 16];
    let (length, file_descriptors) = receiver.receive(&mut payload_buffer).unwrap();
    assert_eq!(&payload_buffer[..length], b"hello");
    assert!(file_descriptors.is_empty());
}
```

Step 3 の実装で通るはずだが、**先にテストを書いて実行し、通ることを確認してから次へ進む**。
通らなければ実装を直す。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-rust/crates/ipc/
git commit -m "feat(ipc): SCM_RIGHTS で fd を運ぶチャネルを追加する"
```

---

### Task 2: ソケットの listen / connect とパス規約

**Files:**
- Modify: `cef-unity-rust/crates/ipc/src/file_descriptor_channel.rs`
- Modify: `cef-unity-rust/crates/ipc/src/lib.rs` (パス生成関数)

**Interfaces:**
- Consumes: Task 1 の `FileDescriptorChannel`
- Produces: `dmabuf_socket_path(server_pid: u32, browser_id: u32) -> String`、
  `FileDescriptorChannel::listen(path: &str) -> std::io::Result<FileDescriptorListener>`、
  `FileDescriptorListener::accept(&self) -> std::io::Result<FileDescriptorChannel>`、
  `FileDescriptorChannel::connect(path: &str) -> std::io::Result<Self>`

- [ ] **Step 1: 失敗するテストを書く**

```rust
#[test]
fn listen_と_connect_で_fd_を渡せる() {
    let path = std::env::temp_dir()
        .join("cef_unity_fd_channel_listen_test.sock")
        .to_string_lossy()
        .into_owned();
    let _ = std::fs::remove_file(&path);
    let listener = FileDescriptorChannel::listen(&path).unwrap();

    let connect_path = path.clone();
    let client_thread = std::thread::spawn(move || {
        let channel = FileDescriptorChannel::connect(&connect_path).unwrap();
        let mut payload_buffer = [0u8; 8];
        let (_length, file_descriptors) = channel.receive(&mut payload_buffer).unwrap();
        file_descriptors.len()
    });

    let accepted = listener.accept().unwrap();
    let file = std::fs::File::open("/dev/null").unwrap();
    accepted.send(b"y", &[std::os::fd::AsRawFd::as_raw_fd(&file)]).unwrap();

    assert_eq!(client_thread.join().unwrap(), 1);
}
```

パス規約のテストも書く (`lib.rs` の `mod tests` に、既存の `shared_memory_flink_path` の
テストがあればその隣へ):

```rust
#[test]
fn dmabuf_ソケットのパスは_pid_と_browser_id_を含む() {
    assert_eq!(
        dmabuf_socket_path(1234, 7),
        "/tmp/cef_unity_dmabuf_1234_7.sock"
    );
}
```

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-ipc --lib'`
Expected: FAIL — `listen` / `connect` / `dmabuf_socket_path` が存在しないためコンパイルエラー

- [ ] **Step 3: 最小実装**

```rust
/// dmabuf の fd を渡すソケットのパス。共有メモリの flink と同じ命名規則。
pub fn dmabuf_socket_path(server_pid: u32, browser_id: u32) -> String {
    format!("/tmp/cef_unity_dmabuf_{}_{}.sock", server_pid, browser_id)
}
```

```rust
pub struct FileDescriptorListener {
    listener: std::os::unix::net::UnixListener,
    path: String,
}

impl FileDescriptorChannel {
    pub fn listen(path: &str) -> std::io::Result<FileDescriptorListener> {
        let _ = std::fs::remove_file(path);
        Ok(FileDescriptorListener {
            listener: std::os::unix::net::UnixListener::bind(path)?,
            path: path.to_string(),
        })
    }

    pub fn connect(path: &str) -> std::io::Result<Self> {
        Ok(Self { socket: std::os::unix::net::UnixStream::connect(path)? })
    }
}

impl FileDescriptorListener {
    pub fn accept(&self) -> std::io::Result<FileDescriptorChannel> {
        let (socket, _address) = self.listener.accept()?;
        Ok(FileDescriptorChannel { socket })
    }
}

impl Drop for FileDescriptorListener {
    fn drop(&mut self) {
        // 残置するとサーバ再起動時に bind が EADDRINUSE で失敗する
        let _ = std::fs::remove_file(&self.path);
    }
}
```

- [ ] **Step 4: 通ることを確認する**

Run: 同上
Expected: PASS (fd チャネルのテスト 3 件すべて)

- [ ] **Step 5: コミット**

```bash
git add cef-unity-rust/crates/ipc/
git commit -m "feat(ipc): dmabuf 用ソケットの listen/connect とパス規約を追加する"
```

---

### Task 3: 出力バッファ記述子の直列化

**Files:**
- Modify: `cef-unity-rust/crates/ipc/src/file_descriptor_channel.rs`

**Interfaces:**
- Consumes: Task 1 の `send` / `receive`
- Produces: `pub struct DmabufDescriptor { pub width: u32, pub height: u32, pub stride: u32, pub modifier: u64, pub fourcc: u32, pub generation: u32 }`、
  `DmabufDescriptor::to_bytes(&self) -> [u8; 28]`、
  `DmabufDescriptor::from_bytes(bytes: &[u8]) -> Option<Self>`

`serde` を使わず固定長のバイト列にする。相手は同一マシンの同一ビルドで、
ペイロードは 1 メッセージ 1 個しかないため、可変長にする理由がない。

- [ ] **Step 1: 失敗するテストを書く**

```rust
#[test]
fn 記述子がバイト列を往復する() {
    let descriptor = DmabufDescriptor {
        width: 1280,
        height: 720,
        stride: 5120,
        modifier: 0x0300_0000_0e08_014,
        fourcc: 0x34325241, // "AR24"
        generation: 3,
    };
    let restored = DmabufDescriptor::from_bytes(&descriptor.to_bytes()).unwrap();
    assert_eq!(restored, descriptor);
}

#[test]
fn 短すぎるバイト列は_none_になる() {
    assert!(DmabufDescriptor::from_bytes(&[0u8; 10]).is_none());
}
```

`#[derive(Debug, PartialEq, Eq, Clone, Copy)]` を付ける。

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-ipc --lib 記述子'`
Expected: FAIL — `DmabufDescriptor` が存在しない

- [ ] **Step 3: 最小実装**

```rust
/// 出力バッファの記述子。fd と一緒に送る。
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct DmabufDescriptor {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub modifier: u64,
    pub fourcc: u32,
    pub generation: u32,
}

pub const DMABUF_DESCRIPTOR_BYTES: usize = 28;

impl DmabufDescriptor {
    pub fn to_bytes(&self) -> [u8; DMABUF_DESCRIPTOR_BYTES] {
        let mut bytes = [0u8; DMABUF_DESCRIPTOR_BYTES];
        bytes[0..4].copy_from_slice(&self.width.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.height.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.stride.to_le_bytes());
        bytes[12..20].copy_from_slice(&self.modifier.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.fourcc.to_le_bytes());
        bytes[24..28].copy_from_slice(&self.generation.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < DMABUF_DESCRIPTOR_BYTES {
            return None;
        }
        Some(Self {
            width: u32::from_le_bytes(bytes[0..4].try_into().ok()?),
            height: u32::from_le_bytes(bytes[4..8].try_into().ok()?),
            stride: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            modifier: u64::from_le_bytes(bytes[12..20].try_into().ok()?),
            fourcc: u32::from_le_bytes(bytes[20..24].try_into().ok()?),
            generation: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
        })
    }
}
```

- [ ] **Step 4: 通ることを確認する**

Run: 同上
Expected: PASS

- [ ] **Step 5: コミット**

```bash
git add cef-unity-rust/crates/ipc/
git commit -m "feat(ipc): dmabuf 記述子の固定長直列化を追加する"
```

---

### Task 4: dmabuf プールの生成と破棄 (C)

**Files:**
- Create: `cef-unity-rust/crates/server/src/dmabuf_pool.c`
- Create: `cef-unity-rust/crates/server/src/dmabuf_pool.rs` (FFI 宣言と安全なラッパー)
- Modify: `cef-unity-rust/crates/server/build.rs`
- Modify: `cef-unity-rust/crates/server/src/main.rs` (`mod dmabuf_pool;`)

**Interfaces:**
- Consumes: なし
- Produces: `DmabufPool::create() -> Option<DmabufPool>` (Rust 側ラッパー)、
  C 側 `void *dmabuf_pool_create(void)` / `void dmabuf_pool_destroy(void *pool)`

GL/GBM のコード自体はユニットテストの対象にしない (macOS の `.m` プール群と同じ扱い)。
Rust 側ラッパーの「構築に失敗したら `None`」という契約だけをテストする。

- [ ] **Step 1: 失敗するテストを書く**

`dmabuf_pool.rs` の `mod tests` に:

```rust
#[test]
fn gpu_がある環境ではプールを構築できる() {
    // DISPLAY もレンダーノードも無い環境 (CI) では構築できないのが正しい挙動なので、
    // レンダーノードの有無で期待値を切り替える。
    let render_node_exists = std::path::Path::new("/dev/dri/renderD128").exists();
    let pool = DmabufPool::create();
    assert_eq!(pool.is_some(), render_node_exists);
}
```

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --bins dmabuf'`
Expected: FAIL — `DmabufPool` が存在しない

- [ ] **Step 3: C 側の最小実装**

`dmabuf_pool.c`。スパイク (`/tmp/dmabuf_render_probe.c` として動作確認済み) の初期化部分が
そのまま使える。

```c
// Linux: CEF の on_accelerated_paint が渡す dmabuf はプールの借用で、
// コールバックを抜けると再利用される。そのため自前の出力バッファへ blit し、
// その dmabuf をクライアントへ渡す。macOS の iosurface_pool.m に相当する。

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <fcntl.h>
#include <gbm.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef struct {
    int drm_file_descriptor;
    struct gbm_device *gbm_device;
    EGLDisplay display;
    EGLContext context;

    struct gbm_bo *output_buffer_object;
    EGLImageKHR output_image;
    unsigned int output_texture;
    unsigned int output_framebuffer;
    int output_width;
    int output_height;
    unsigned int generation;

    PFNEGLCREATEIMAGEKHRPROC create_image;
    PFNEGLDESTROYIMAGEKHRPROC destroy_image;
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target_texture;
} DmabufPool;

void *dmabuf_pool_create(void) {
    DmabufPool *pool = calloc(1, sizeof(DmabufPool));
    if (!pool) return NULL;

    pool->drm_file_descriptor = open("/dev/dri/renderD128", O_RDWR);
    if (pool->drm_file_descriptor < 0) { free(pool); return NULL; }

    pool->gbm_device = gbm_create_device(pool->drm_file_descriptor);
    if (!pool->gbm_device) { close(pool->drm_file_descriptor); free(pool); return NULL; }

    PFNEGLGETPLATFORMDISPLAYEXTPROC get_platform_display =
        (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
    if (!get_platform_display) { dmabuf_pool_destroy(pool); return NULL; }
    pool->display = get_platform_display(EGL_PLATFORM_GBM_KHR, pool->gbm_device, NULL);
    if (pool->display == EGL_NO_DISPLAY) { dmabuf_pool_destroy(pool); return NULL; }

    EGLint major = 0, minor = 0;
    if (!eglInitialize(pool->display, &major, &minor)) { dmabuf_pool_destroy(pool); return NULL; }
    eglBindAPI(EGL_OPENGL_ES_API);

    EGLint config_attributes[] = {EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
                                  EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
                                  EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
                                  EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE};
    EGLConfig config;
    EGLint config_count = 0;
    eglChooseConfig(pool->display, config_attributes, &config, 1, &config_count);
    EGLint context_attributes[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    pool->context = eglCreateContext(pool->display, config, EGL_NO_CONTEXT, context_attributes);
    if (pool->context == EGL_NO_CONTEXT) { dmabuf_pool_destroy(pool); return NULL; }
    if (!eglMakeCurrent(pool->display, EGL_NO_SURFACE, EGL_NO_SURFACE, pool->context)) {
        dmabuf_pool_destroy(pool); return NULL;
    }

    pool->create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    pool->destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    pool->image_target_texture =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if (!pool->create_image || !pool->destroy_image || !pool->image_target_texture) {
        dmabuf_pool_destroy(pool); return NULL;
    }
    return pool;
}

void dmabuf_pool_destroy(void *handle) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool) return;
    if (pool->output_framebuffer) glDeleteFramebuffers(1, &pool->output_framebuffer);
    if (pool->output_texture) glDeleteTextures(1, &pool->output_texture);
    if (pool->output_image && pool->destroy_image)
        pool->destroy_image(pool->display, pool->output_image);
    if (pool->output_buffer_object) gbm_bo_destroy(pool->output_buffer_object);
    if (pool->context != EGL_NO_CONTEXT) {
        eglMakeCurrent(pool->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        eglDestroyContext(pool->display, pool->context);
    }
    if (pool->display != EGL_NO_DISPLAY) eglTerminate(pool->display);
    if (pool->gbm_device) gbm_device_destroy(pool->gbm_device);
    if (pool->drm_file_descriptor >= 0) close(pool->drm_file_descriptor);
    free(pool);
}
```

`dmabuf_pool_destroy` は `dmabuf_pool_create` より前に前方宣言を置くこと。

- [ ] **Step 4: build.rs に登録する**

```rust
#[cfg(target_os = "linux")]
{
    cc::Build::new()
        .file("src/dmabuf_pool.c")
        .compile("dmabuf_pool");
    println!("cargo:rustc-link-lib=gbm");
    println!("cargo:rustc-link-lib=EGL");
    println!("cargo:rustc-link-lib=GLESv2");
}
```

既存の macOS 分岐と同じ形で `#[cfg]` を分ける。

- [ ] **Step 5: Rust 側ラッパーを書く**

```rust
//! Linux: dmabuf プールの安全なラッパー。C 実装は dmabuf_pool.c。

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn dmabuf_pool_create() -> *mut std::ffi::c_void;
    fn dmabuf_pool_destroy(pool: *mut std::ffi::c_void);
}

#[cfg(target_os = "linux")]
pub struct DmabufPool {
    handle: *mut std::ffi::c_void,
}

#[cfg(target_os = "linux")]
impl DmabufPool {
    /// 構築できなければ `None`。呼び出し側は software paint に落ちる
    /// (Windows の `d3d11_pool.is_some()` と同じ判定の仕方)。
    pub fn create() -> Option<Self> {
        let handle = unsafe { dmabuf_pool_create() };
        if handle.is_null() { None } else { Some(Self { handle }) }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DmabufPool {
    fn drop(&mut self) {
        unsafe { dmabuf_pool_destroy(self.handle) };
    }
}
```

`main.rs` に `#[cfg(target_os = "linux")] mod dmabuf_pool;` を足す。

- [ ] **Step 6: 通ることを確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --bins dmabuf'`
Expected: PASS

macOS でもビルドが壊れていないことを確認する:

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust && cargo build --release
```

- [ ] **Step 7: コミット**

```bash
git add cef-unity-rust/crates/server/
git commit -m "feat(server): Linux の dmabuf プールの生成と破棄を追加する"
```

---

### Task 5: CEF dmabuf の取り込みと出力バッファへの blit

**Files:**
- Modify: `cef-unity-rust/crates/server/src/dmabuf_pool.c`
- Modify: `cef-unity-rust/crates/server/src/dmabuf_pool.rs`

**Interfaces:**
- Consumes: Task 4 の `DmabufPool`
- Produces: `DmabufPool::blit(&self, source: DmabufSource) -> bool`、
  `DmabufPool::output(&self) -> Option<(std::os::fd::OwnedFd, cef_unity_ipc::file_descriptor_channel::DmabufDescriptor)>`、
  `pub struct DmabufSource { pub file_descriptor: RawFd, pub stride: u32, pub modifier: u64, pub fourcc: u32, pub width: u32, pub height: u32 }`

- [ ] **Step 1: 失敗するテストを書く**

自前で作った dmabuf を「CEF が渡してきたもの」に見立てて blit し、出力記述子が
取れることを確かめる。GPU が無い環境ではスキップする。

```rust
#[test]
fn blit_すると出力記述子が取れる() {
    let Some(pool) = DmabufPool::create() else {
        return; // GPU の無い環境 (CI) ではプールが作れないので検証対象外
    };
    let source = DmabufPool::create_test_source(&pool, 256, 256)
        .expect("テスト用のソースバッファを確保できない");
    assert!(pool.blit(source));

    let (_file_descriptor, descriptor) = pool.output().expect("出力記述子が取れない");
    assert_eq!(descriptor.width, 256);
    assert_eq!(descriptor.height, 256);
    assert!(descriptor.stride >= 256 * 4);
    assert_eq!(descriptor.generation, 1);
}
```

`create_test_source` は C 側に `dmabuf_pool_create_test_source` として実装する
(GBM で確保して fd と stride/modifier を返すだけ)。テスト専用の入口だが、
本番コードから呼ばれないため `#[cfg(test)]` で囲む。

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --bins dmabuf'`
Expected: FAIL — `blit` / `output` が存在しない

- [ ] **Step 3: C 側の実装**

出力バッファの確保 (サイズが変わったときだけ再作成し `generation` を進める):

```c
static int ensure_output(DmabufPool *pool, int width, int height) {
    if (pool->output_buffer_object && pool->output_width == width
        && pool->output_height == height) {
        return 1;
    }
    if (pool->output_framebuffer) { glDeleteFramebuffers(1, &pool->output_framebuffer); pool->output_framebuffer = 0; }
    if (pool->output_texture) { glDeleteTextures(1, &pool->output_texture); pool->output_texture = 0; }
    if (pool->output_image) { pool->destroy_image(pool->display, pool->output_image); pool->output_image = NULL; }
    if (pool->output_buffer_object) { gbm_bo_destroy(pool->output_buffer_object); pool->output_buffer_object = NULL; }

    // リニア modifier は NVIDIA の GBM で確保に失敗するため要求しない
    pool->output_buffer_object = gbm_bo_create(pool->gbm_device, width, height,
                                               GBM_FORMAT_ARGB8888, GBM_BO_USE_RENDERING);
    if (!pool->output_buffer_object) return 0;

    int output_file_descriptor = gbm_bo_get_fd(pool->output_buffer_object);
    EGLint image_attributes[] = {
        EGL_WIDTH, width, EGL_HEIGHT, height,
        EGL_LINUX_DRM_FOURCC_EXT, GBM_FORMAT_ARGB8888,
        EGL_DMA_BUF_PLANE0_FD_EXT, output_file_descriptor,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)gbm_bo_get_stride(pool->output_buffer_object),
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,
            (EGLint)(gbm_bo_get_modifier(pool->output_buffer_object) & 0xffffffff),
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,
            (EGLint)(gbm_bo_get_modifier(pool->output_buffer_object) >> 32),
        EGL_NONE};
    pool->output_image = pool->create_image(pool->display, EGL_NO_CONTEXT,
                                            EGL_LINUX_DMA_BUF_EXT, NULL, image_attributes);
    close(output_file_descriptor); // EGLImage が参照を保持するのでここでは閉じてよい
    if (pool->output_image == EGL_NO_IMAGE_KHR) return 0;

    glGenTextures(1, &pool->output_texture);
    glBindTexture(GL_TEXTURE_2D, pool->output_texture);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    pool->image_target_texture(GL_TEXTURE_2D, pool->output_image);

    glGenFramebuffers(1, &pool->output_framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, pool->output_framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                           pool->output_texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return 0;

    pool->output_width = width;
    pool->output_height = height;
    pool->generation += 1;
    return 1;
}
```

blit 本体。全画面クアッドをテクスチャ付きで描く。頂点/フラグメントシェーダは
プール構築時に 1 度だけコンパイルして保持する (`ensure_program`)。

```c
int dmabuf_pool_blit(void *handle, int source_file_descriptor, unsigned int source_stride,
                     unsigned long long source_modifier, unsigned int source_fourcc,
                     int width, int height) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool || !ensure_output(pool, width, height) || !ensure_program(pool)) return 0;

    EGLint image_attributes[] = {
        EGL_WIDTH, width, EGL_HEIGHT, height,
        EGL_LINUX_DRM_FOURCC_EXT, (EGLint)source_fourcc,
        EGL_DMA_BUF_PLANE0_FD_EXT, source_file_descriptor,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)source_stride,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(source_modifier & 0xffffffff),
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(source_modifier >> 32),
        EGL_NONE};
    EGLImageKHR source_image = pool->create_image(pool->display, EGL_NO_CONTEXT,
                                                  EGL_LINUX_DMA_BUF_EXT, NULL, image_attributes);
    if (source_image == EGL_NO_IMAGE_KHR) return 0;

    unsigned int source_texture = 0;
    glGenTextures(1, &source_texture);
    glBindTexture(GL_TEXTURE_2D, source_texture);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    pool->image_target_texture(GL_TEXTURE_2D, source_image);

    glBindFramebuffer(GL_FRAMEBUFFER, pool->output_framebuffer);
    glViewport(0, 0, width, height);
    draw_full_screen_quad(pool, source_texture);
    // macOS の waitUntilCompleted と同じ位置づけ。完了を待ってから frame_id を進める
    glFinish();

    glDeleteTextures(1, &source_texture);
    pool->destroy_image(pool->display, source_image);
    return 1;
}

int dmabuf_pool_output(void *handle, int *out_file_descriptor, unsigned int *out_stride,
                       unsigned long long *out_modifier, unsigned int *out_fourcc,
                       unsigned int *out_generation, int *out_width, int *out_height) {
    DmabufPool *pool = (DmabufPool *)handle;
    if (!pool || !pool->output_buffer_object) return 0;
    *out_file_descriptor = gbm_bo_get_fd(pool->output_buffer_object); // 呼び出し側が閉じる
    *out_stride = gbm_bo_get_stride(pool->output_buffer_object);
    *out_modifier = gbm_bo_get_modifier(pool->output_buffer_object);
    *out_fourcc = GBM_FORMAT_ARGB8888;
    *out_generation = pool->generation;
    *out_width = pool->output_width;
    *out_height = pool->output_height;
    return 1;
}
```

- [ ] **Step 4: Rust 側ラッパーを足す**

`blit` と `output` を FFI 宣言し、`output` は `OwnedFd` と `DmabufDescriptor` を返す。
`cef-unity-ipc` を `crates/server/Cargo.toml` の依存に追加する (既に入っていれば不要)。

- [ ] **Step 5: 通ることを確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --bins dmabuf'`
Expected: PASS

- [ ] **Step 6: リサイズで generation が進むテストを足す**

```rust
#[test]
fn サイズが変わると_generation_が進む() {
    let Some(pool) = DmabufPool::create() else { return };
    let first = DmabufPool::create_test_source(&pool, 256, 256).unwrap();
    assert!(pool.blit(first));
    let (_fd1, descriptor1) = pool.output().unwrap();

    let second = DmabufPool::create_test_source(&pool, 512, 512).unwrap();
    assert!(pool.blit(second));
    let (_fd2, descriptor2) = pool.output().unwrap();

    assert_eq!(descriptor2.generation, descriptor1.generation + 1);
    assert_eq!(descriptor2.width, 512);
}

#[test]
fn 同じサイズなら_generation_は進まない() {
    let Some(pool) = DmabufPool::create() else { return };
    let first = DmabufPool::create_test_source(&pool, 256, 256).unwrap();
    assert!(pool.blit(first));
    let (_fd1, descriptor1) = pool.output().unwrap();

    let second = DmabufPool::create_test_source(&pool, 256, 256).unwrap();
    assert!(pool.blit(second));
    let (_fd2, descriptor2) = pool.output().unwrap();

    assert_eq!(descriptor2.generation, descriptor1.generation);
}
```

RED を確認してから Step 3 の `ensure_output` を必要に応じて直す。

- [ ] **Step 7: コミット**

```bash
git add cef-unity-rust/crates/server/
git commit -m "feat(server): CEF の dmabuf を出力バッファへ blit する"
```

---

### Task 6: server.rs への統合

**Files:**
- Modify: `cef-unity-rust/crates/server/src/server.rs`
- Modify: `cef-unity-rust/crates/ipc/src/lib.rs` (`Bootstrap` にソケットパス)

**Interfaces:**
- Consumes: Task 2 の `dmabuf_socket_path` / `FileDescriptorChannel`、Task 5 の `DmabufPool`
- Produces: なし (内部結線)

- [ ] **Step 1: 判定にプール構築を足すテストを書く**

```rust
#[test]
fn プールが無ければ_accelerated_paint_は無効() {
    assert!(!linux_accelerated_paint_available(true, Some(":0"), false));
}

#[test]
fn 三条件が揃えば有効() {
    assert!(linux_accelerated_paint_available(true, Some(":0"), true));
}
```

既存の 4 テストも第 3 引数 `true` を渡す形に更新する。

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --bins'`
Expected: FAIL — 引数の数が合わずコンパイルエラー

- [ ] **Step 3: 判定を拡張する**

```rust
fn linux_accelerated_paint_available(
    use_gpu: bool,
    display: Option<&str>,
    pool_available: bool,
) -> bool {
    use_gpu && display.is_some_and(|value| !value.is_empty()) && pool_available
}
```

`linux_use_accelerated_paint` は `DmabufPool` の構築結果を渡すように変える。
プールはブラウザ生成前に 1 度だけ構築し、`CefServer` が保持する。

- [ ] **Step 4: 通ることを確認する**

Run: 同上
Expected: PASS

- [ ] **Step 5: on_accelerated_paint を blit の呼び出しに置き換える**

Task 5 までの「破棄する」実装を消し、`dmabuf_pool.blit(...)` を呼ぶ。成功したら
`SharedMemoryWriter` のヘッダへ `frame_id` と `generation` を書く。

出力バッファが再作成された (= `generation` が変わった) ときだけ、
`FileDescriptorChannel::send(descriptor.to_bytes(), &[output_file_descriptor])` で送る。

- [ ] **Step 6: Bootstrap にソケットパスを載せる**

`Bootstrap` に `dmabuf_socket_path: Option<String>` を足し、GPU 経路が有効なときだけ
値を入れる。無効なときは `None` で、クライアントは接続を試みない。

- [ ] **Step 7: ビルドと既存テストの確認**

```bash
ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test --workspace --lib --bins'
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust && cargo test --workspace --lib --bins --release
```

Expected: 両方 PASS (macOS のビルドを壊していないこと)

- [ ] **Step 8: コミット**

```bash
git add cef-unity-rust/
git commit -m "feat(server): dmabuf プールを accelerated paint 経路へ結線する"
```

---

### Task 7: クライアント側の取り込み

**Files:**
- Create: `cef-unity-rust/crates/client/src/dmabuf.rs`
- Modify: `cef-unity-rust/crates/client/src/lib.rs`
- Modify: `cef-unity-rust/crates/client/build.rs` (EGL / GLESv2 のリンク)

**Interfaces:**
- Consumes: Task 3 の `DmabufDescriptor`、Task 2 の `FileDescriptorChannel::connect`
- Produces: `cef_unity_get_dmabuf_texture(handle: *mut CefUnityBrowser) -> u32` (GL テクスチャ名。0 なら未取得)

- [ ] **Step 1: 世代整合のテストを書く**

GL を要さない純粋な部分だけをテストする。「ヘッダの generation に一致するテクスチャだけを使う」
という規則が本体。

```rust
#[test]
fn 世代が一致しなければテクスチャを返さない() {
    let mut cache = DmabufTextureCache::new();
    cache.store(3, 42); // generation=3 のテクスチャ名 42
    assert_eq!(cache.texture_for_generation(3), Some(42));
    assert_eq!(cache.texture_for_generation(4), None);
}

#[test]
fn 新しい世代を格納すると古い世代は返らなくなる() {
    let mut cache = DmabufTextureCache::new();
    cache.store(3, 42);
    cache.store(4, 43);
    assert_eq!(cache.texture_for_generation(4), Some(43));
    assert_eq!(cache.texture_for_generation(3), None);
}
```

- [ ] **Step 2: 失敗を確認する**

Run: `ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-client --lib dmabuf'`
Expected: FAIL — `DmabufTextureCache` が存在しない

- [ ] **Step 3: 最小実装**

```rust
/// 受け取った dmabuf の GL テクスチャを世代付きで保持する。
/// 共有メモリヘッダの generation と一致するものだけを使う (spec の 2-b)。
pub struct DmabufTextureCache {
    generation: Option<u32>,
    texture: u32,
}

impl DmabufTextureCache {
    pub fn new() -> Self { Self { generation: None, texture: 0 } }

    pub fn store(&mut self, generation: u32, texture: u32) {
        self.generation = Some(generation);
        self.texture = texture;
    }

    pub fn texture_for_generation(&self, generation: u32) -> Option<u32> {
        if self.generation == Some(generation) { Some(self.texture) } else { None }
    }
}
```

- [ ] **Step 4: 通ることを確認する**

Run: 同上
Expected: PASS

- [ ] **Step 5: GL 取り込みと FFI を足す**

`eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` → `glEGLImageTargetTexture2DOES` の呼び出しを
書く。**ホストの GL コンテキストが current なスレッドからのみ呼ぶこと。**
`d3d11.rs` と同じく呼び出しスレッド ID を記録し、変化したら WARNING をログに出す。

古い世代のテクスチャと EGLImage と fd は、新しい世代を格納する時点で解放する。

- [ ] **Step 6: csbindgen の再生成**

```bash
cd /Users/juha/Documents/GitHub/cef-unity/cef-unity-rust && cargo build --release
```

`cef-unity-csharp/CefUnity.Core/Interop/NativeMethods.g.cs` が更新されることを確認する。

- [ ] **Step 7: コミット**

```bash
git add cef-unity-rust/ cef-unity-csharp/CefUnity.Core/Interop/
git commit -m "feat(client): dmabuf を GL テクスチャとして取り込む"
```

---

### Task 8: Viewer の OpenGL レンダラ

**Files:**
- Create: `cef-unity-csharp/CefUnity.Viewer/OpenGLFrameRenderer.cs`
- Modify: `cef-unity-csharp/CefUnity.Viewer/FrameRendererFactory.cs`
- Modify: `cef-unity-csharp/CefUnity.Viewer/CefUnity.Viewer.csproj` (`Silk.NET.OpenGL`)
- Test: `cef-unity-csharp/CefUnity.Tests/FrameRendererFactoryTests.cs` (既存があれば追記)

**Interfaces:**
- Consumes: Task 7 の `cef_unity_get_dmabuf_texture`
- Produces: `FrameRendererKind.OpenGL`

- [ ] **Step 1: ファクトリのテストを書く**

```csharp
[Test]
public void Linux では OpenGL を選ぶ()
{
    Assert.That(
        FrameRendererFactory.SelectKind(isMacOS: false, isWindows: false, isLinux: true),
        Is.EqualTo(FrameRendererKind.OpenGL));
}

[Test]
public void どれでもなければ Unsupported のまま()
{
    Assert.That(
        FrameRendererFactory.SelectKind(isMacOS: false, isWindows: false, isLinux: false),
        Is.EqualTo(FrameRendererKind.Unsupported));
}
```

既存の macOS / Windows のテストがあれば、引数追加に合わせて更新する。

- [ ] **Step 2: 失敗を確認する**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests --filter FrameRenderer`
Expected: FAIL — 引数の数が合わずコンパイルエラー

- [ ] **Step 3: ファクトリを拡張する**

```csharp
public static FrameRendererKind SelectKind(bool isMacOS, bool isWindows, bool isLinux)
{
    if (isMacOS) return FrameRendererKind.Metal;
    if (isWindows) return FrameRendererKind.Direct3D11;
    if (isLinux) return FrameRendererKind.OpenGL;
    return FrameRendererKind.Unsupported;
}

public static FrameRendererKind SelectKind()
    => SelectKind(
        RuntimeInformation.IsOSPlatform(OSPlatform.OSX),
        RuntimeInformation.IsOSPlatform(OSPlatform.Windows),
        RuntimeInformation.IsOSPlatform(OSPlatform.Linux));
```

`FrameRendererKind` に `OpenGL` を足す。

- [ ] **Step 4: 通ることを確認する**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests --filter FrameRenderer`
Expected: PASS

- [ ] **Step 5: OpenGLFrameRenderer を実装する**

`D3D11FrameRenderer.cs` と同じ構造。`Present(IntPtr texturePointer, int width, int height)` の
`texturePointer` には GL テクスチャ名が入る (`IntPtr.Zero` なら blit せず drawable を回すだけ、
という既存の契約は維持する)。全画面クアッドで既定フレームバッファへ描く。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-csharp/
git commit -m "feat(viewer): Linux の OpenGL 表示バックエンドを追加する"
```

---

### Task 9: lifecycle に fd リーク検出を足す

**Files:**
- Modify: `cef-unity-csharp/CefUnity.Harness/LifecycleCommand.cs`

**Interfaces:**
- Consumes: なし
- Produces: なし

- [ ] **Step 1: fd 数を数える関数のテストを書く**

```csharp
[Test]
public void 開いているファイル記述子を数えられる()
{
    var before = LifecycleCommand.CountOpenFileDescriptors();
    using var file = File.Open("/dev/null", FileMode.Open);
    var after = LifecycleCommand.CountOpenFileDescriptors();
    Assert.That(after, Is.GreaterThan(before));
}
```

Linux 以外では `-1` を返す契約にし、テストは Linux でのみ実行する
(`[Platform("Linux")]` またはランタイム判定で早期 return)。

- [ ] **Step 2: 失敗を確認する**

Run: `dotnet test cef-unity-csharp/CefUnity.Tests --filter FileDescriptor`
Expected: FAIL — `CountOpenFileDescriptors` が存在しない

- [ ] **Step 3: 最小実装**

```csharp
/// <summary>Linux で開いているファイル記述子の数。他プラットフォームでは -1。</summary>
public static int CountOpenFileDescriptors()
{
    if (!RuntimeInformation.IsOSPlatform(OSPlatform.Linux)) return -1;
    try { return Directory.GetFileSystemEntries("/proc/self/fd").Length; }
    catch (DirectoryNotFoundException) { return -1; }
}
```

- [ ] **Step 4: 通ることを確認する**

Run: 同上
Expected: PASS

- [ ] **Step 5: サイクルごとの出力に足す**

macOS が `mach_ports=` を出している位置に `file_descriptors=` を追記する。
`-1` のときは表示しない。

- [ ] **Step 6: コミット**

```bash
git add cef-unity-csharp/
git commit -m "feat(harness): lifecycle に fd リーク検出を足す"
```

---

### Task 10: 統合テストと実機確認

**Files:**
- Create: `cef-unity-rust/crates/server/tests/dmabuf_accelerated_paint.rs`

**Interfaces:**
- Consumes: これまでの全タスク
- Produces: なし

既存の `crates/server/tests/ime_caret_tracking.rs` と同じ形にする。**CEF サーバの残留は
次のテストを永久ハングさせるため、`Drop` で shutdown + kill する仕組みを必ず踏襲すること。**
複数の `cargo test` プロセスを並行実行しないこと (CEF は同時 1 インスタンス)。

- [ ] **Step 1: 統合テストを書く**

GPU 経路でブラウザを起動し、既知の色を全面に描くページ (`data:` URL) を読み込み、
出力 dmabuf を読み戻して色を検証する。`DISPLAY` が無い環境では早期 return する。

- [ ] **Step 2: 失敗を確認してから通す**

Run:
```bash
ssh ubuntu 'cd ~/cef-unity-build/cef-unity-rust && DISPLAY=:0 XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1) CARGO_TARGET_DIR=$HOME/cef-target PATH=$HOME/.cargo/bin:$PATH cargo test -p cef-unity-server --test dmabuf_accelerated_paint'
```

- [ ] **Step 3: Viewer で目視確認する (本フェーズの合否判定)**

```bash
ssh ubuntu 'cd ~/cef-unity-build && DISPLAY=:0 XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1) dotnet run --project cef-unity-csharp/CefUnity.Viewer -c Release'
```

スクリーンショットを撮って、ページが正しく表示されていることを確認する。

- [ ] **Step 4: フォールバックの回帰を確認する**

```bash
ssh ubuntu 'cd ~/harness && env -u DISPLAY ./CefUnity.Harness smoke'
```

Expected: `SMOKE_OK frames=N` (N > 0) — software paint に落ちて従来どおり動くこと

- [ ] **Step 5: fd リークを確認する**

```bash
ssh ubuntu 'cd ~/harness && DISPLAY=:0 XAUTHORITY=$(ls /run/user/1000/.mutter-Xwaylandauth* | head -1) ./CefUnity.Harness lifecycle 5'
```

Expected: 5/5 サイクル完走、`file_descriptors=` が横ばい

- [ ] **Step 6: マージ前の性能計測**

`paint-statistics 20 1920 1080 animation` / `zero-frame-wait 20 10 1920 1080 intermittent` /
`lifecycle 5` を Linux と macOS の双方で回し、結果を `docs/HARNESS_MEASUREMENTS.md` に記録する
(リポジトリの恒久ルール)。

- [ ] **Step 7: ドキュメントを更新してコミット**

`cef-unity-rust/CLAUDE.md` のサポート対象表と機能対応表で、Linux の GPU ゼロコピーを
「実装中 (main 未投入)」から実際の状態へ更新する。

```bash
git add .
git commit -m "test(server): dmabuf accelerated paint の統合テストを追加する"
```

---

## Self-Review

**Spec coverage:**

| spec の変更点 | 対応タスク |
|---|---|
| 1. `dmabuf_pool.c` | Task 4, 5 |
| 2. `file_descriptor_channel.rs` | Task 1, 2, 3 |
| 2-b. リサイズ時の世代整合 | Task 5 (generation の進行)、Task 7 (クライアント側の規則) |
| 3. `server.rs` | Task 6 |
| 4. `client/dmabuf.rs` | Task 7 |
| 5. Viewer の OpenGL レンダラ | Task 8 |
| 6. `lifecycle` の fd 計測 | Task 9 |
| 検証手順 1〜6 | Task 10 |

**未解決として残すもの:** spec の「未知のリスク」に挙げた 3 点 (CEF のリニア dmabuf の
取り込み可否、dmabuf の有効期間、`glFinish` のコスト) は Task 5 と Task 10 で実データに
当たって初めて判明する。Task 5 で取り込みが失敗した場合は、`modifier` を無視して
インポートする経路や、CEF 側に modifier を指定させる方法を検討する必要がある。
