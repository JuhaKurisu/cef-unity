// IOSurface pool with Metal GPU blit for server-side copy.
//
// CEF reuses a small set of IOSurfaces (triple-buffered). We copy the source
// IOSurface to a pool surface via Metal blit so that CEF can safely reuse the
// source for the next frame.
//
// Synchronization: waitUntilCompleted ensures the blit is fully done before
// returning the pool surface to the caller (for Mach IPC transfer).
//
// 同期版と非同期版の 2 経路がある。非同期版 (iosurface_pool_copy_async) は
// 完了待ちを CEF の message pump から外すためのもので、環境変数
// CEF_UNITY_ASYNC_COPY=1 のときに server.rs が選ぶ (issue #7)。

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <stdatomic.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
#include <stdio.h>

#define POOL_SIZE 5
#define SRC_CACHE_SIZE 4

// 非同期モードで同時に GPU へ投げてよいコピーの上限。
// pump を止めなくなると CEF は 60paint/s を出し続けるため、GPU が消化できない
// ときは in-flight が溜まって POOL_SIZE を踏み抜く。上限を超えた paint は捨てる
// (GPU 飢餓時の挙動を「pump 凍結」から「フレームドロップ」に変える)。
#define MAX_IN_FLIGHT_COPIES 2

/// 非同期コピーの完了通知。blit 完了後に呼ばれ、この時点で surface は転送して安全。
typedef void (*iosurface_pool_completion_callback)(void* surface, uint32_t width,
                                                   uint32_t height, uint32_t format);

static id<MTLDevice> g_device = nil;
static id<MTLCommandQueue> g_queue = nil;
static IOSurfaceRef g_pool[POOL_SIZE] = {NULL};
static id<MTLTexture> g_dst_tex[POOL_SIZE] = {nil};  // cached dst textures
static int g_pool_idx = 0;
static uint32_t g_pool_w = 0;
static uint32_t g_pool_h = 0;

// Source texture cache (CEF rotates through 2-3 IOSurfaces)
static struct {
    IOSurfaceRef surface;
    id<MTLTexture> texture;
} g_src_cache[SRC_CACHE_SIZE];
static int g_src_cache_count = 0;

/// Lazily initialize the Metal device and command queue.
static int ensure_metal(void) {
    if (g_device != nil) return 1;
    g_device = MTLCreateSystemDefaultDevice();
    if (g_device == nil) {
        fprintf(stderr, "[iosurface_pool] MTLCreateSystemDefaultDevice failed\n");
        return 0;
    }
    g_queue = [g_device newCommandQueue];
    if (g_queue == nil) {
        fprintf(stderr, "[iosurface_pool] newCommandQueue failed\n");
        g_device = nil;
        return 0;
    }
    return 1;
}

/// Create an IOSurface suitable for GPU blit destination.
static IOSurfaceRef create_pool_surface(uint32_t w, uint32_t h) {
    NSDictionary *props = @{
        (id)kIOSurfaceWidth:            @(w),
        (id)kIOSurfaceHeight:           @(h),
        (id)kIOSurfaceBytesPerElement:  @(4),
        (id)kIOSurfacePixelFormat:      @((uint32_t)'BGRA'),
    };
    return IOSurfaceCreate((__bridge CFDictionaryRef)props);
}

/// Invalidate all pool surfaces and cached textures (called on dimension change).
static void invalidate_pool(void) {
    for (int i = 0; i < POOL_SIZE; i++) {
        if (g_pool[i] != NULL) {
            CFRelease(g_pool[i]);
            g_pool[i] = NULL;
        }
        g_dst_tex[i] = nil;
    }
    for (int i = 0; i < g_src_cache_count; i++) {
        g_src_cache[i].surface = NULL;
        g_src_cache[i].texture = nil;
    }
    g_src_cache_count = 0;
    g_pool_idx = 0;
}

/// Look up or create a Metal texture for an IOSurface (src side).
static id<MTLTexture> get_src_texture(IOSurfaceRef surface, uint32_t w, uint32_t h) {
    // Check cache (CEF typically rotates 2-3 surfaces)
    for (int i = 0; i < g_src_cache_count; i++) {
        if (g_src_cache[i].surface == surface) {
            return g_src_cache[i].texture;
        }
    }

    // Cache miss: create new texture
    MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                   width:w
                                                                                  height:h
                                                                               mipmapped:NO];
    desc.storageMode = MTLStorageModeShared;
    desc.usage = MTLTextureUsageShaderRead;

    id<MTLTexture> tex = [g_device newTextureWithDescriptor:desc iosurface:surface plane:0];
    if (!tex) return nil;

    // Add to cache (evict oldest if full)
    int slot;
    if (g_src_cache_count < SRC_CACHE_SIZE) {
        slot = g_src_cache_count++;
    } else {
        for (int i = 0; i < SRC_CACHE_SIZE - 1; i++)
            g_src_cache[i] = g_src_cache[i + 1];
        slot = SRC_CACHE_SIZE - 1;
    }
    g_src_cache[slot].surface = surface;
    g_src_cache[slot].texture = tex;
    return tex;
}

/// 転送先プール surface と、src/dst の Metal テクスチャを用意する。
/// 成功時 1 を返し、out 引数を埋める。
static int prepare_copy(IOSurfaceRef src, uint32_t w, uint32_t h,
                        IOSurfaceRef *out_dst, id<MTLTexture> *out_src_texture,
                        id<MTLTexture> *out_dst_texture) {
    // Recreate pool on dimension change
    if (w != g_pool_w || h != g_pool_h) {
        invalidate_pool();
        g_pool_w = w;
        g_pool_h = h;
    }

    // Get or create the destination surface + cached texture
    int idx = g_pool_idx;
    if (g_pool[idx] == NULL) {
        g_pool[idx] = create_pool_surface(w, h);
        if (g_pool[idx] == NULL) {
            fprintf(stderr, "[iosurface_pool] create_pool_surface failed\n");
            return 0;
        }
        g_dst_tex[idx] = nil;
    }
    IOSurfaceRef dst = g_pool[idx];
    g_pool_idx = (g_pool_idx + 1) % POOL_SIZE;

    // Get cached textures
    id<MTLTexture> srcTex = get_src_texture(src, w, h);
    if (!srcTex) return 0;

    if (!g_dst_tex[idx]) {
        MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                       width:w
                                                                                      height:h
                                                                                   mipmapped:NO];
        desc.storageMode = MTLStorageModeShared;
        desc.usage = MTLTextureUsageShaderWrite;
        g_dst_tex[idx] = [g_device newTextureWithDescriptor:desc iosurface:dst plane:0];
        if (!g_dst_tex[idx]) return 0;
    }

    *out_dst = dst;
    *out_src_texture = srcTex;
    *out_dst_texture = g_dst_tex[idx];
    return 1;
}

/// blit を 1 つの command buffer に encode する (commit はしない)。
/// completion handler は commit 前にしか追加できないため、commit は呼び出し側で行う。
static id<MTLCommandBuffer> encode_blit(id<MTLTexture> srcTex, id<MTLTexture> dstTex,
                                        uint32_t w, uint32_t h) {
    id<MTLCommandBuffer> cmdBuf = [g_queue commandBuffer];
    if (cmdBuf == nil) return nil;

    id<MTLBlitCommandEncoder> blit = [cmdBuf blitCommandEncoder];
    [blit copyFromTexture:srcTex
              sourceSlice:0
              sourceLevel:0
             sourceOrigin:(MTLOrigin){0, 0, 0}
               sourceSize:(MTLSize){w, h, 1}
                toTexture:dstTex
         destinationSlice:0
         destinationLevel:0
        destinationOrigin:(MTLOrigin){0, 0, 0}];
    [blit endEncoding];
    return cmdBuf;
}

/// Copy src IOSurface → pool IOSurface via synchronous Metal blit.
/// Returns the pool IOSurfaceRef with completed blit (safe for Mach IPC transfer).
///
/// ⚠ この完了待ちは CEF の message pump スレッド上で行われるため、外部 GPU 競合下では
/// pump 自体が数十〜数百 ms 止まる (issue #7)。非同期版は iosurface_pool_copy_async。
void* iosurface_pool_copy_and_get(void* src_ref, uint32_t w, uint32_t h, uint32_t format __attribute__((unused))) {
    if (src_ref == NULL || w == 0 || h == 0) return NULL;
    if (!ensure_metal()) return NULL;

    IOSurfaceRef dst = NULL;
    id<MTLTexture> srcTex = nil;
    id<MTLTexture> dstTex = nil;
    if (!prepare_copy((IOSurfaceRef)src_ref, w, h, &dst, &srcTex, &dstTex)) return NULL;

    // Synchronous GPU blit: copy src → dst, wait for completion.
    // Cost ~0.5ms on Apple Silicon (GPU blit <0.1ms + scheduling overhead).
    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = encode_blit(srcTex, dstTex, w, h);
        if (cmdBuf == nil) return NULL;
        [cmdBuf commit];
        [cmdBuf waitUntilCompleted];
    }

    return (void*)dst;
}

/// 検証用の既知不良構成: blit を commit しただけで完了を待たずに転送先を返す
/// (過去の試行2 と同じ)。client が blit 未完了の surface を読むため、ティアリング
/// 検出器が反応することの確認 (negative control) にのみ使う。
/// CEF_UNITY_UNSAFE_NO_WAIT=1 のときだけ server.rs が呼ぶ。
void* iosurface_pool_copy_no_wait_unsafe(void* src_ref, uint32_t w, uint32_t h,
                                         uint32_t format __attribute__((unused))) {
    if (src_ref == NULL || w == 0 || h == 0) return NULL;
    if (!ensure_metal()) return NULL;

    IOSurfaceRef dst = NULL;
    id<MTLTexture> srcTex = nil;
    id<MTLTexture> dstTex = nil;
    if (!prepare_copy((IOSurfaceRef)src_ref, w, h, &dst, &srcTex, &dstTex)) return NULL;

    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = encode_blit(srcTex, dstTex, w, h);
        if (cmdBuf == nil) return NULL;
        [cmdBuf commit];
    }
    return (void*)dst;
}

// ---------------------------------------------------------------------------
// 非同期コピー (issue #7 の修正候補)
// ---------------------------------------------------------------------------
//
// 設計: blit を encode + commit して即 return し、完了ハンドラで「送っていい」と
// 通知する。client が surface を受け取るのは blit 完了後だけなので、過去に失敗した
// 試行 (待ちを消す・status を見る・前フレームを返す) と違い、転送先が未完了のまま
// 読まれることは構造的に起きない。Windows 経路 (d3d11_pool.rs) が CopyResource +
// fence で既に採っている形の macOS 版に当たる。
//
// 残るリスクは「CEF がコールバック復帰後に src をプールへ戻し、GPU がまだ読んでいる」
// こと。これは Windows 経路が現に出荷している条件と同じで、CEF 側のプール深度
// (2-3 枚 ≒ 33ms) が猶予になる。

static iosurface_pool_completion_callback g_completion_callback = NULL;
static _Atomic int g_in_flight_copies = 0;
/// 送信を直列化するキュー。完了ハンドラは Metal 内部スレッドで走るためブロックさせず、
/// ここへ渡す。serial なので commit 順 = 送信順が保たれる。
static dispatch_queue_t g_send_queue = NULL;

void iosurface_pool_set_completion_callback(iosurface_pool_completion_callback callback) {
    g_completion_callback = callback;
    if (g_send_queue == NULL) {
        g_send_queue = dispatch_queue_create("com.cef-unity.iosurface-send",
                                             DISPATCH_QUEUE_SERIAL);
    }
}

/// src → pool の blit を投げるだけで返る。完了時に completion callback が呼ばれる。
/// 戻り値: 1 = 投入した / 0 = in-flight 上限でこのフレームを捨てた / -1 = エラー。
int iosurface_pool_copy_async(void* src_ref, uint32_t w, uint32_t h, uint32_t format) {
    if (src_ref == NULL || w == 0 || h == 0) return -1;
    if (!ensure_metal()) return -1;
    if (g_completion_callback == NULL || g_send_queue == NULL) return -1;

    if (atomic_load(&g_in_flight_copies) >= MAX_IN_FLIGHT_COPIES) {
        return 0; // backpressure: GPU が追いついていないのでこの paint は捨てる
    }

    IOSurfaceRef dst = NULL;
    id<MTLTexture> srcTex = nil;
    id<MTLTexture> dstTex = nil;
    if (!prepare_copy((IOSurfaceRef)src_ref, w, h, &dst, &srcTex, &dstTex)) return -1;

    atomic_fetch_add(&g_in_flight_copies, 1);

    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = encode_blit(srcTex, dstTex, w, h);
        if (cmdBuf == nil) {
            atomic_fetch_sub(&g_in_flight_copies, 1);
            return -1;
        }
        // ⚠ completion handler は commit より前に追加すること。commit 後に
        // addCompletedHandler を呼ぶと Metal が例外を投げてプロセスが落ちる (実測)。
        iosurface_pool_completion_callback callback = g_completion_callback;
        [cmdBuf addCompletedHandler:^(id<MTLCommandBuffer> completed) {
            (void)completed;
            // ハンドラは Metal 内部スレッドで走る。ブロックしないよう送信は直列キューへ。
            dispatch_async(g_send_queue, ^{
                callback((void*)dst, w, h, format);
                atomic_fetch_sub(&g_in_flight_copies, 1);
            });
        }];
        [cmdBuf commit];
    }

    return 1;
}
