// IOSurface pool with Metal GPU blit for server-side copy.
//
// CEF reuses a small set of IOSurfaces (triple-buffered). We copy the source
// IOSurface to a pool surface via Metal blit so that CEF can safely reuse the
// source for the next frame.
//
// Synchronization strategy (no waitUntilCompleted):
//   - CEF rotates 3 source IOSurfaces, so a given source is reused only after
//     2+ frames (~32ms). Our GPU blit executes in < 1ms, well before reuse.
//   - The destination pool IOSurface has kernel-level GPU tracking. When the
//     client creates a Metal texture from it, the GPU automatically waits for
//     our blit to finish before reading (cross-process IOSurface sync).
//   - This eliminates 1.5ms of thread blocking per frame vs waitUntilCompleted.

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <stdint.h>
#include <stdio.h>
#include <mach/mach_time.h>

#define POOL_SIZE 5
#define SRC_CACHE_SIZE 4

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

// Profiling
static mach_timebase_info_data_t g_timebase = {0, 0};
static uint64_t g_blit_count = 0;
static double g_total_ms = 0;

static double ticks_to_ms(uint64_t elapsed) {
    if (g_timebase.denom == 0) mach_timebase_info(&g_timebase);
    return (double)elapsed * g_timebase.numer / g_timebase.denom / 1e6;
}

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

/// Copy src IOSurface → pool IOSurface via Metal blit.
/// Returns the pool IOSurfaceRef (owned by the pool, do NOT release).
/// Returns NULL on failure.
void* iosurface_pool_copy_and_get(void* src_ref, uint32_t w, uint32_t h, uint32_t format __attribute__((unused))) {
    if (src_ref == NULL || w == 0 || h == 0) return NULL;
    if (!ensure_metal()) return NULL;

    IOSurfaceRef src = (IOSurfaceRef)src_ref;

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
            return NULL;
        }
        g_dst_tex[idx] = nil;  // invalidate cached texture
    }
    IOSurfaceRef dst = g_pool[idx];
    g_pool_idx = (g_pool_idx + 1) % POOL_SIZE;

    // Get cached textures (avoids per-frame Obj-C allocations)
    id<MTLTexture> srcTex = get_src_texture(src, w, h);
    if (!srcTex) return NULL;

    if (!g_dst_tex[idx]) {
        MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                       width:w
                                                                                      height:h
                                                                                   mipmapped:NO];
        desc.storageMode = MTLStorageModeShared;
        desc.usage = MTLTextureUsageShaderWrite;
        g_dst_tex[idx] = [g_device newTextureWithDescriptor:desc iosurface:dst plane:0];
        if (!g_dst_tex[idx]) return NULL;
    }
    id<MTLTexture> dstTex = g_dst_tex[idx];

    // Blit + wait (only Obj-C allocs per frame: commandBuffer + blitEncoder)
    uint64_t t0 = mach_absolute_time();

    @autoreleasepool {
        id<MTLCommandBuffer> cmdBuf = [g_queue commandBuffer];
        if (cmdBuf == nil) return NULL;

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

        [cmdBuf commit];
        // No waitUntilCompleted:
        // - src IOSurface: CEF rotates 3 sources; our blit (<1ms GPU) finishes
        //   well before CEF reuses this source 2 frames later (~32ms).
        // - dst IOSurface: kernel-level IOSurface tracking ensures the client's
        //   GPU waits for our blit to complete before reading (cross-process sync).
    }

    uint64_t t1 = mach_absolute_time();
    g_blit_count++;
    g_total_ms += ticks_to_ms(t1 - t0);
    if (g_blit_count % 300 == 0) {
        NSString *tmpDir = NSTemporaryDirectory();
        NSString *logPath = [tmpDir stringByAppendingPathComponent:@"cef_unity_server.log"];
        FILE *f = fopen([logPath UTF8String], "a");
        if (f) {
            fprintf(f, "[iosurface_pool] blit avg=%.3fms over 300 frames (total %.1fms)\n",
                    g_total_ms / 300.0, g_total_ms);
            fclose(f);
        }
        g_total_ms = 0;
    }

    return (void*)dst;
}
