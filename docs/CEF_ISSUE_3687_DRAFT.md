# Draft: comment for chromiumembedded/cef issue #3687

Title context: "osr: linux: Add cefclient implementation for OnAcceleratedPaint"

---

We tested the Linux `OnAcceleratedPaint` path on CEF 145.5.0 (Chromium 145.0.7632.117)
and found that **the callback fires reliably, but the exported dmabuf contents are
always zero** on NVIDIA. Details below in case they are useful for anyone implementing
the cefclient side or debugging the export path.

## Environment

- Ubuntu 26.04.1, kernel 7.0.0-30-generic, X11 ozone (`--ozone-platform=x11`)
- GeForce RTX 3060 Ti, nvidia-driver-595-open (595.84)
- `windowless_rendering_enabled=1`, `shared_texture_enabled=1`,
  `external_begin_frame_enabled=1`, `--enable-features=Vulkan`
- chrome://gpu confirms: Compositing/Rasterization Hardware accelerated,
  Skia Backend = GaneshVulkan, ANGLE = OpenGL, GPU process crash count 0

## What works

- `OnAcceleratedPaint` fires at 60 calls/sec with a valid single-plane
  native pixmap: `modifier=0x0 (LINEAR)`, `stride=5120`, `offset=0`,
  plane `size=3686400` for 1280x720, and a genuine dmabuf fd
  (`readlink /proc/self/fd/N` → `/dmabuf:`).
- The software path (`shared_texture_enabled=0` → `OnPaint`) renders the same
  page correctly on the same machine, so CEF/network/driver are otherwise fine.

## What does not work

The dmabuf contents are all-zero on every frame. We read the buffer through
three independent routes, all returning zero for every byte of every sampled
frame over 10 seconds (animated page, so damage was continuous):

1. CPU: `mmap` + `DMA_BUF_IOCTL_SYNC` — 0 non-zero bytes out of 3.6 MB
2. GL: `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` + `GL_TEXTURE_EXTERNAL_OES` — samples (0,0,0,0)
3. Vulkan: import via `VK_EXT_image_drm_format_modifier` (LINEAR) +
   `vkCmdCopyImageToBuffer` — 0 non-zero bytes

Since Chromium's compositor writes with Vulkan (GaneshVulkan) and a Vulkan
import in another process still reads zero, the write itself appears to never
reach the exported memory. On the viz side,
`FrameSinkVideoCapturerImpl` does issue the `BlitRequest`
(`populates_gpu_memory_buffer=true`) and delivers the frame as a success, so
the failure is silent.

## Things we ruled out

- Reading too early (checked 1-frame-delayed reads; also sampled frames 0..600)
- visible_rect/content_rect offsets (all full-frame)
- external begin frame (same result with it disabled)
- modifier attribute presence in EGL import (same either way)
- `--use-angle=vulkan`, `use-vulkan=native` (callback fires, still zero)
- `SkiaGraphite`, `DefaultANGLEVulkan`, `in-process-gpu` (these stop the
  callback from firing entirely)

Happy to provide the harness we used (it drives CEF headfully via X11 and
reads the dmabuf via CPU/GL/Vulkan each frame).
