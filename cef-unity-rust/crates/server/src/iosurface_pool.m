// IOSurface pool with Metal GPU blit for server-side copy.
//
// CEF reuses a small set of IOSurfaces (triple-buffered). The surface passed to
// on_accelerated_paint is only valid for the duration of the callback. We must
// copy the content before returning, per CEF 124+ spec:
//   "You must ensure your copy has completed before returning from the callback"
//
// This module maintains a pool of IOSurfaces and uses Metal's blit encoder to
// perform a GPU-side copy from the CEF source surface to a pool surface.
// The pool surface is then sent via Mach IPC to the client.

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <stdint.h>
#include <stdio.h>

#define POOL_SIZE 3

static id<MTLDevice> g_device = nil;
static id<MTLCommandQueue> g_queue = nil;
static IOSurfaceRef g_pool[POOL_SIZE] = {NULL};
static int g_pool_idx = 0;
static uint32_t g_pool_w = 0;
static uint32_t g_pool_h = 0;

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

/// Invalidate all pool surfaces (called on dimension change).
static void invalidate_pool(void) {
    for (int i = 0; i < POOL_SIZE; i++) {
        if (g_pool[i] != NULL) {
            CFRelease(g_pool[i]);
            g_pool[i] = NULL;
        }
    }
    g_pool_idx = 0;
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

    // Get or create the destination surface
    int idx = g_pool_idx;
    if (g_pool[idx] == NULL) {
        g_pool[idx] = create_pool_surface(w, h);
        if (g_pool[idx] == NULL) {
            fprintf(stderr, "[iosurface_pool] create_pool_surface failed\n");
            return NULL;
        }
    }
    IOSurfaceRef dst = g_pool[idx];
    g_pool_idx = (g_pool_idx + 1) % POOL_SIZE;

    // @autoreleasepool: Metal objects (commandBuffer, blitCommandEncoder, textures)
    // are autoreleased. Without a pool, they accumulate until a periodic drain,
    // causing frame-time spikes.
    @autoreleasepool {
        MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                                                       width:w
                                                                                      height:h
                                                                                   mipmapped:NO];
        desc.storageMode = MTLStorageModeShared;  // IOSurface-backed
        desc.usage = MTLTextureUsageShaderRead;

        id<MTLTexture> srcTex = [g_device newTextureWithDescriptor:desc iosurface:src plane:0];
        if (srcTex == nil) {
            fprintf(stderr, "[iosurface_pool] newTextureWithDescriptor (src) failed\n");
            return NULL;
        }

        desc.usage = MTLTextureUsageShaderWrite;
        id<MTLTexture> dstTex = [g_device newTextureWithDescriptor:desc iosurface:dst plane:0];
        if (dstTex == nil) {
            fprintf(stderr, "[iosurface_pool] newTextureWithDescriptor (dst) failed\n");
            return NULL;
        }

        // Blit copy
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
        [cmdBuf waitUntilCompleted];

        if (cmdBuf.status == MTLCommandBufferStatusError) {
            fprintf(stderr, "[iosurface_pool] Metal blit failed: %s\n",
                    [[cmdBuf.error localizedDescription] UTF8String]);
            return NULL;
        }
    }

    return (void*)dst;
}
