// IOSurface pool with Metal GPU blit for server-side copy.
//
// CEF reuses a small set of IOSurfaces (triple-buffered). We copy the source
// IOSurface to a pool surface via Metal blit so that CEF can safely reuse the
// source for the next frame.
//
// 同期版と非同期版の 2 経路がある。
//
// - 非同期版 (iosurface_pool_copy_async): 既定。blit を投げるだけで返り、完了ハンドラで
//   送信する。完了待ちが CEF の message pump を止めないようにするため (issue #7)。
// - 同期版 (iosurface_pool_copy_and_get): waitUntilCompleted で完了を待ってから返す。
//   CEF_UNITY_SYNC_COPY=1 で戻せる従来経路。待ちは pump スレッド上で起きる。

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <os/lock.h>
#include <stdatomic.h>
#include <dispatch/dispatch.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#define POOL_SIZE 5
#define SRC_CACHE_SIZE 4

// 同時に追跡できる in-flight コピーの上限 (安全網)。通常は BeginFrame ゲート
// (server.rs) が in-flight を 1 以下に保つので、ここまで溜まることはない。
#define MAX_IN_FLIGHT_COPIES 4

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

// Source texture cache.
//
// ⚠ キーは IOSurfaceRef ポインタではなく IOSurfaceID にすること。CEF は同じ surface に
// 対しても毎回異なる IOSurfaceRef を渡してくる (実測: id=315 に対しポインタが
// 0x11409cffe40 / 0x11409abbd00 / 0x1140a2316c0 と毎回変わる)。ポインタで引くと
// 毎フレーム cache miss して MTLTexture を作り直すことになる。
// テクスチャが surface を retain するため、生きている間に ID が別の surface へ
// 再割り当てされることはない。
static struct {
    IOSurfaceID surface_id;
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
///
/// 非同期コピーが実行中でも安全: BeginFrame ゲートにより on_accelerated_paint の時点で
/// in-flight は 0 なので通常ここに実行中の blit は無く、仮にあっても Metal の command
/// buffer が参照するテクスチャを完了まで retain するため解放は起きない
/// (retainedReferences 既定 = YES)。
static void invalidate_pool(void) {
    for (int i = 0; i < POOL_SIZE; i++) {
        if (g_pool[i] != NULL) {
            CFRelease(g_pool[i]);
            g_pool[i] = NULL;
        }
        g_dst_tex[i] = nil;
    }
    for (int i = 0; i < g_src_cache_count; i++) {
        g_src_cache[i].surface_id = 0;
        g_src_cache[i].texture = nil;
    }
    g_src_cache_count = 0;
    g_pool_idx = 0;
}

/// Look up or create a Metal texture for an IOSurface (src side).
static id<MTLTexture> get_src_texture(IOSurfaceRef surface, uint32_t w, uint32_t h) {
    IOSurfaceID surface_id = IOSurfaceGetID(surface);
    for (int i = 0; i < g_src_cache_count; i++) {
        if (g_src_cache[i].surface_id == surface_id) {
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
    g_src_cache[slot].surface_id = surface_id;
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

// 検証用の低速コピー。CEF_UNITY_SLOW_COPY=n で 1 コピーあたりの「転送元を読んでいる
// 時間」を人為的に広げる。転送元を 16 バンドに分けて順に転送し、バンドの合間に
// 全面ダミー blit を n 回挟む。CEF が読み取り中の転送元を上書きすれば、バンドごとに
// 内容が食い違う = ティアリングとして現れる。
// BeginFrame ゲート (server.rs) が本当に上書きを防いでいるかを確かめる positive
// control 専用で、実運用では 0 (無効)。
#define SLOW_COPY_BANDS 16

static int g_slow_copy_repeats = -1;
static id<MTLTexture> g_scratch_texture = nil;
static uint32_t g_scratch_width = 0;
static uint32_t g_scratch_height = 0;

static int slow_copy_repeats(void) {
    if (g_slow_copy_repeats < 0) {
        const char* value = getenv("CEF_UNITY_SLOW_COPY");
        int parsed = value != NULL ? atoi(value) : 0;
        g_slow_copy_repeats = parsed > 0 ? parsed : 0;
    }
    return g_slow_copy_repeats;
}

/// ダミー blit の転送先。内容は使わないので private storage でよい。
static id<MTLTexture> ensure_scratch_texture(uint32_t w, uint32_t h) {
    if (g_scratch_texture != nil && g_scratch_width == w && g_scratch_height == h) {
        return g_scratch_texture;
    }
    MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                    width:w
                                                                                   height:h
                                                                                mipmapped:NO];
    desc.storageMode = MTLStorageModePrivate;
    desc.usage = MTLTextureUsageShaderWrite;
    g_scratch_texture = [g_device newTextureWithDescriptor:desc];
    g_scratch_width = w;
    g_scratch_height = h;
    return g_scratch_texture;
}

/// 全面を 1 回で転送する通常の encode。
static void encode_full_copy(id<MTLBlitCommandEncoder> blit, id<MTLTexture> srcTex,
                             id<MTLTexture> dstTex, uint32_t w, uint32_t h) {
    [blit copyFromTexture:srcTex
              sourceSlice:0
              sourceLevel:0
             sourceOrigin:(MTLOrigin){0, 0, 0}
               sourceSize:(MTLSize){w, h, 1}
                toTexture:dstTex
         destinationSlice:0
         destinationLevel:0
        destinationOrigin:(MTLOrigin){0, 0, 0}];
}

/// blit を 1 つの command buffer に encode する (commit はしない)。
/// completion handler は commit 前にしか追加できないため、commit は呼び出し側で行う。
static id<MTLCommandBuffer> encode_blit(id<MTLTexture> srcTex, id<MTLTexture> dstTex,
                                        uint32_t w, uint32_t h) {
    id<MTLCommandBuffer> cmdBuf = [g_queue commandBuffer];
    if (cmdBuf == nil) return nil;

    id<MTLBlitCommandEncoder> blit = [cmdBuf blitCommandEncoder];
    int repeats = slow_copy_repeats();
    id<MTLTexture> scratch = repeats > 0 ? ensure_scratch_texture(w, h) : nil;
    if (repeats > 0 && scratch != nil) {
        for (uint32_t band = 0; band < SLOW_COPY_BANDS; band++) {
            uint32_t y = h * band / SLOW_COPY_BANDS;
            uint32_t band_height = h * (band + 1) / SLOW_COPY_BANDS - y;
            if (band_height == 0) continue;
            [blit copyFromTexture:srcTex
                      sourceSlice:0
                      sourceLevel:0
                     sourceOrigin:(MTLOrigin){0, y, 0}
                       sourceSize:(MTLSize){w, band_height, 1}
                        toTexture:dstTex
                 destinationSlice:0
                 destinationLevel:0
                destinationOrigin:(MTLOrigin){0, y, 0}];
            for (int repeat = 0; repeat < repeats; repeat++) {
                encode_full_copy(blit, srcTex, scratch, w, h);
            }
        }
    } else {
        encode_full_copy(blit, srcTex, dstTex, w, h);
    }
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
// 非同期コピー (issue #7 の修正)
// ---------------------------------------------------------------------------
//
// 設計: blit を encode + commit して即 return し、完了ハンドラで「送っていい」と
// 通知する。client が転送先 surface を受け取るのは blit 完了後だけなので、過去に
// 失敗した試行 (待ちを消す・status を見る・前フレームを返す) のような「転送先が
// 未完了のまま読まれる」失敗は構造的に起きない。
//
// ただし CEF は転送元について明示的な契約を置いている
// (cef_render_handler_capi.h): "The handle's resource cannot be accessed outside
// of this callback. The contents of |info| will be released back to the pool
// after this callback returns." つまりコールバックから返った後に GPU が転送元を
// 読んでいると、CEF が同じ IOSurface へ次のフレームを描き込んで内容が混ざり得る。
//
// これを守るための不変条件は 2 つで、破れないことを in-flight 追跡で保証する:
//
//   1. 転送元を読んでいる blit がある間は CEF に新しいフレームを描かせない。
//      CEF の描画は BeginFrame でしか起きず、BeginFrame は pump 上でしか処理され
//      ないので、server.rs 側で「in-flight が 0 でなければ BeginFrame を発行しない」
//      ゲートを置けば足りる (pump は止めない = issue #7 の要求)。
//      「1 枚だけ先行させる」ことはできない: CEF の転送元プールの枚数は固定でなく
//      (競合下では十数枚に増えるのを実測)、返却済みの surface を次フレームに再び
//      選ぶ可能性を排除できないため。直列でも供給レートは同期版と変わらない
//      (同期版も 1 コピー完了ごとに 1 フレームしか進まない)。
//   2. 万一 1 が破れて CEF が読み取り中の転送元を再交付した場合、その blit の
//      結果は送らずに捨てる (poison)。ゲートが効いていれば発生しない防御であり、
//      発生件数は統計に出して検証できるようにしてある。

static iosurface_pool_completion_callback g_completion_callback = NULL;
/// 送信を直列化するキュー。完了ハンドラは Metal 内部スレッドで走るためブロックさせず、
/// ここへ渡す。serial なので commit 順 = 送信順が保たれる。
static dispatch_queue_t g_send_queue = NULL;

/// 実行中 (= 転送元をまだ読んでいる可能性がある) の blit。
/// 転送元の識別は IOSurfaceID で行う (ポインタは毎回変わる。g_src_cache のコメント参照)。
static struct {
    IOSurfaceID source_id;  ///< 0 = 空きスロット
    int poisoned;           ///< CEF が同じ転送元を再交付した = 内容が混ざった疑い
} g_in_flight[MAX_IN_FLIGHT_COPIES];
static _Atomic int g_in_flight_copies = 0;
/// poison されて破棄した blit の総数 (診断用。ゲートが効いていれば 0)。
static _Atomic uint64_t g_poisoned_copies = 0;
/// g_in_flight を触る pump スレッドと Metal 完了スレッドの排他。
static os_unfair_lock g_in_flight_lock = OS_UNFAIR_LOCK_INIT;

void iosurface_pool_set_completion_callback(iosurface_pool_completion_callback callback) {
    g_completion_callback = callback;
    if (g_send_queue == NULL) {
        g_send_queue = dispatch_queue_create("com.cef-unity.iosurface-send",
                                             DISPATCH_QUEUE_SERIAL);
    }
}

/// 転送元をまだ読んでいる可能性がある blit の数。
int iosurface_pool_in_flight_copies(void) {
    return atomic_load(&g_in_flight_copies);
}

/// poison されて破棄した blit の総数。
uint64_t iosurface_pool_poisoned_copies(void) {
    return atomic_load(&g_poisoned_copies);
}

/// CEF が転送元 src を再交付した = src へ次のフレームを描き込んだ、という通知。
/// src をまだ読んでいる blit があればその結果を捨てる印を付ける。印を付けた数を返す。
int iosurface_pool_poison_copies_reading(void* src_ref) {
    if (src_ref == NULL) return 0;
    IOSurfaceID source_id = IOSurfaceGetID((IOSurfaceRef)src_ref);
    if (source_id == 0) return 0;
    int poisoned = 0;
    os_unfair_lock_lock(&g_in_flight_lock);
    for (int index = 0; index < MAX_IN_FLIGHT_COPIES; index++) {
        if (g_in_flight[index].source_id == source_id) {
            g_in_flight[index].poisoned = 1;
            poisoned++;
        }
    }
    os_unfair_lock_unlock(&g_in_flight_lock);
    return poisoned;
}

/// 空きスロットを確保して転送元を記録する。空きが無ければ -1。
static int acquire_in_flight_slot(IOSurfaceID source_id) {
    int slot = -1;
    os_unfair_lock_lock(&g_in_flight_lock);
    for (int index = 0; index < MAX_IN_FLIGHT_COPIES; index++) {
        if (g_in_flight[index].source_id == 0) {
            g_in_flight[index].source_id = source_id;
            g_in_flight[index].poisoned = 0;
            slot = index;
            break;
        }
    }
    os_unfair_lock_unlock(&g_in_flight_lock);
    return slot;
}

/// スロットを解放し、poison されていたかを返す。
static int release_in_flight_slot(int slot) {
    os_unfair_lock_lock(&g_in_flight_lock);
    int poisoned = g_in_flight[slot].poisoned;
    g_in_flight[slot].source_id = 0;
    g_in_flight[slot].poisoned = 0;
    os_unfair_lock_unlock(&g_in_flight_lock);
    return poisoned;
}

/// src → pool の blit を投げるだけで返る。完了時に completion callback が呼ばれる。
/// 戻り値: 1 = 投入した / 0 = 追跡スロット枯渇でこのフレームを捨てた / -1 = エラー。
int iosurface_pool_copy_async(void* src_ref, uint32_t w, uint32_t h, uint32_t format) {
    if (src_ref == NULL || w == 0 || h == 0) return -1;
    if (!ensure_metal()) return -1;
    if (g_completion_callback == NULL || g_send_queue == NULL) return -1;

    IOSurfaceRef dst = NULL;
    id<MTLTexture> srcTex = nil;
    id<MTLTexture> dstTex = nil;
    if (!prepare_copy((IOSurfaceRef)src_ref, w, h, &dst, &srcTex, &dstTex)) return -1;

    int slot = acquire_in_flight_slot(IOSurfaceGetID((IOSurfaceRef)src_ref));
    if (slot < 0) return 0; // 安全網: ゲートが効いていればここには来ない
    atomic_fetch_add(&g_in_flight_copies, 1);

    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = encode_blit(srcTex, dstTex, w, h);
        if (cmdBuf == nil) {
            release_in_flight_slot(slot);
            atomic_fetch_sub(&g_in_flight_copies, 1);
            return -1;
        }
        // ⚠ completion handler は commit より前に追加すること。commit 後に
        // addCompletedHandler を呼ぶと Metal が例外を投げてプロセスが落ちる (実測)。
        iosurface_pool_completion_callback callback = g_completion_callback;
        [cmdBuf addCompletedHandler:^(id<MTLCommandBuffer> completed) {
            (void)completed;
            // ここで転送元の読み出しは完了している。CEF が再利用しても安全になったので
            // 送信 (直列キュー) を待たずに in-flight を減らし、BeginFrame ゲートを開ける。
            int poisoned = release_in_flight_slot(slot);
            atomic_fetch_sub(&g_in_flight_copies, 1);
            if (poisoned) {
                atomic_fetch_add(&g_poisoned_copies, 1);
                return; // 転送元が上書きされた疑いがあるフレームは送らない
            }
            // ハンドラは Metal 内部スレッドで走る。ブロックしないよう送信は直列キューへ。
            dispatch_async(g_send_queue, ^{
                callback((void*)dst, w, h, format);
            });
        }];
        [cmdBuf commit];
    }

    return 1;
}
