// Metal/IOSurface bridge for CEF-Unity GPU texture sharing.
//
// Two modes:
// 1. Legacy: IOSurfaceLookup (broken on macOS 16 cross-process)
// 2. Mach port: IOSurfaceLookupFromMachPort (works cross-process)

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#import <Foundation/Foundation.h>
#import <mach/mach.h>
#import <servers/bootstrap.h>
#import <mach/mach_time.h>

// Profiling
static mach_timebase_info_data_t _timebase = {0, 0};
static uint64_t _prof_call_count = 0;
static double _prof_drain_total_ms = 0;
static double _prof_lookup_total_ms = 0;
static double _prof_tex_create_total_ms = 0;
static double _prof_total_ms = 0;
static int _prof_drain_msg_total = 0;
static int _prof_cache_hits = 0;
static int _prof_cache_misses = 0;

static double ticks_to_ms(uint64_t elapsed) {
    if (_timebase.denom == 0) mach_timebase_info(&_timebase);
    return (double)elapsed * _timebase.numer / _timebase.denom / 1e6;
}

static id<MTLDevice> _sharedDevice = nil;
static id<MTLCommandQueue> _sharedQueue = nil;

// IOSurface テクスチャ + sRGB view キャッシュ (IOSurfaceID で比較、マルチエントリ)
#define IOSURFACE_CACHE_SIZE 4
static struct {
    IOSurfaceID surfaceID;
    IOSurfaceRef surface;
    id<MTLTexture> srgbView;
} _surfaceCache[IOSURFACE_CACHE_SIZE];
static int _surfaceCacheCount = 0;

// Private テクスチャ (安定コピー先、ダブルバッファ)
static id<MTLTexture> _privateTex[2] = {nil, nil};
static int _privateWriteIdx = 0;
static uint32_t _privateW = 0, _privateH = 0;
static MTLPixelFormat _privateFormat = MTLPixelFormatInvalid;

// ---------------------------------------------------------------------------
// Mach port IOSurface client
// ---------------------------------------------------------------------------

// Must match server's iosurface_msg_t layout
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t surface_port;
    uint32_t width;
    uint32_t height;
    uint32_t format;
} iosurface_msg_t;

// Subscribe message (client → server)
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t client_port;
} subscribe_msg_t;

static mach_port_t g_receive_port = MACH_PORT_NULL;

/// Connect to the server's Mach IOSurface service and send subscription.
/// Returns 0 on success, negative on error.
int mach_iosurface_client_connect(const char* service_name) {
    kern_return_t kr;
    mach_port_t server_port;

    kr = bootstrap_look_up(bootstrap_port, service_name, &server_port);
    if (kr != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] bootstrap_look_up('%s') failed: %s", service_name, mach_error_string(kr));
        return -1;
    }

    // Create our receive port
    kr = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &g_receive_port);
    if (kr != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] mach_port_allocate failed: %s", mach_error_string(kr));
        mach_port_deallocate(mach_task_self(), server_port);
        return -2;
    }

    // Send subscription message with our port (as a send right)
    subscribe_msg_t msg;
    __builtin_memset(&msg, 0, sizeof(msg));

    msg.header.msgh_bits = MACH_MSGH_BITS_COMPLEX |
                           MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    msg.header.msgh_size = sizeof(msg);
    msg.header.msgh_remote_port = server_port;
    msg.header.msgh_local_port = MACH_PORT_NULL;
    msg.header.msgh_id = 0x53554253;  // 'SUBS'

    msg.body.msgh_descriptor_count = 1;

    msg.client_port.name = g_receive_port;
    msg.client_port.disposition = MACH_MSG_TYPE_MAKE_SEND;
    msg.client_port.type = MACH_MSG_PORT_DESCRIPTOR;

    kr = mach_msg(
        &msg.header,
        MACH_SEND_MSG | MACH_SEND_TIMEOUT,
        sizeof(msg),
        0,
        MACH_PORT_NULL,
        5000,  // 5s timeout
        MACH_PORT_NULL
    );

    mach_port_deallocate(mach_task_self(), server_port);

    if (kr != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] subscribe send failed: %s", mach_error_string(kr));
        return -3;
    }

    NSLog(@"[CefUnity-Mach] connected to '%s', receive port=%u", service_name, g_receive_port);
    return 0;
}

/// Non-blocking receive of IOSurface from Mach port channel.
/// On success, returns an MTLTexture pointer (retained, caller must release).
/// On no message or error, returns NULL.
void* mach_iosurface_recv_texture(int32_t* out_width, int32_t* out_height, uint32_t* out_format) {
    if (g_receive_port == MACH_PORT_NULL) return NULL;

    uint64_t t_start = mach_absolute_time();

    // Drain all pending messages, keep only the latest
    IOSurfaceRef latest_surface = NULL;
    uint32_t latest_width = 0, latest_height = 0, latest_format = 0;
    int drain_count = 0;

    uint64_t t_drain_start = mach_absolute_time();
    for (;;) {
        struct {
            iosurface_msg_t msg;
            mach_msg_trailer_t trailer;
        } recv_buf;
        __builtin_memset(&recv_buf, 0, sizeof(recv_buf));

        kern_return_t kr = mach_msg(
            &recv_buf.msg.header,
            MACH_RCV_MSG | MACH_RCV_TIMEOUT,
            0,
            sizeof(recv_buf),
            g_receive_port,
            0,
            MACH_PORT_NULL
        );

        if (kr != MACH_MSG_SUCCESS) break;

        mach_port_t surface_port = recv_buf.msg.surface_port.name;
        IOSurfaceRef surface = IOSurfaceLookupFromMachPort(surface_port);
        mach_port_deallocate(mach_task_self(), surface_port);

        if (surface) {
            if (latest_surface) CFRelease(latest_surface);
            latest_surface = surface;
            latest_width = recv_buf.msg.width;
            latest_height = recv_buf.msg.height;
            latest_format = recv_buf.msg.format;
        }
        drain_count++;
    }
    uint64_t t_drain_end = mach_absolute_time();

    if (!latest_surface) return NULL;

    // Ensure Metal device
    if (!_sharedDevice) {
        _sharedDevice = MTLCreateSystemDefaultDevice();
        if (!_sharedDevice) {
            NSLog(@"[CefUnity-Mach] MTLCreateSystemDefaultDevice() returned nil");
            CFRelease(latest_surface);
            return NULL;
        }
        NSLog(@"[CefUnity-Mach] Metal device: %@", _sharedDevice.name);
    }

    uint64_t t_tex_start = mach_absolute_time();

    // マルチエントリキャッシュで IOSurfaceID を検索
    IOSurfaceID latestID = IOSurfaceGetID(latest_surface);
    id<MTLTexture> srcSrgbView = nil;
    int cacheHit = 0;

    for (int i = 0; i < _surfaceCacheCount; i++) {
        if (_surfaceCache[i].surfaceID == latestID && _surfaceCache[i].srgbView) {
            CFRelease(latest_surface);
            srcSrgbView = _surfaceCache[i].srgbView;
            cacheHit = 1;
            break;
        }
    }

    if (!srcSrgbView) {
        // キャッシュミス: IOSurface テクスチャを作成
        MTLPixelFormat iosfFormat = (latest_format == 1)
            ? MTLPixelFormatRGBA8Unorm
            : MTLPixelFormatBGRA8Unorm;

        MTLTextureDescriptor *desc = [MTLTextureDescriptor
            texture2DDescriptorWithPixelFormat:iosfFormat
                                         width:(NSUInteger)latest_width
                                        height:(NSUInteger)latest_height
                                     mipmapped:NO];
        desc.usage = MTLTextureUsageShaderRead | MTLTextureUsagePixelFormatView;
        desc.storageMode = MTLStorageModeShared;

        id<MTLTexture> iosTex = [_sharedDevice newTextureWithDescriptor:desc
                                                              iosurface:latest_surface
                                                                  plane:0];
        if (!iosTex) {
            NSLog(@"[CefUnity-Mach] newTextureWithDescriptor:iosurface: returned nil");
            CFRelease(latest_surface);
            return NULL;
        }

        MTLPixelFormat srgbFormat = (latest_format == 1)
            ? MTLPixelFormatRGBA8Unorm_sRGB
            : MTLPixelFormatBGRA8Unorm_sRGB;

        srcSrgbView = [iosTex newTextureViewWithPixelFormat:srgbFormat];
        if (!srcSrgbView) srcSrgbView = iosTex;

        // キャッシュに追加
        int slot;
        if (_surfaceCacheCount < IOSURFACE_CACHE_SIZE) {
            slot = _surfaceCacheCount++;
        } else {
            if (_surfaceCache[0].surface) CFRelease(_surfaceCache[0].surface);
            for (int i = 0; i < IOSURFACE_CACHE_SIZE - 1; i++)
                _surfaceCache[i] = _surfaceCache[i + 1];
            slot = IOSURFACE_CACHE_SIZE - 1;
        }
        _surfaceCache[slot].surfaceID = latestID;
        _surfaceCache[slot].surface = latest_surface;
        _surfaceCache[slot].srgbView = srcSrgbView;
    }

    // --- optimize + Private blit: ロスレス圧縮してから安定コピーを作成 ---
    // 1. optimizeContentsForGPUAccess: Shared テクスチャの GPU 読み取りを高速化
    // 2. copyFromTexture: 圧縮済み Shared → Private にコピー (安定スナップショット)
    // Private コピーにより CEF が IOSurface を再利用しても Unity の描画に影響しない
    MTLPixelFormat targetFormat = (latest_format == 1)
        ? MTLPixelFormatRGBA8Unorm_sRGB
        : MTLPixelFormatBGRA8Unorm_sRGB;

    if (!_privateTex[0] || _privateW != latest_width || _privateH != latest_height || _privateFormat != targetFormat) {
        MTLTextureDescriptor *privDesc = [MTLTextureDescriptor
            texture2DDescriptorWithPixelFormat:targetFormat
                                         width:(NSUInteger)latest_width
                                        height:(NSUInteger)latest_height
                                     mipmapped:NO];
        privDesc.usage = MTLTextureUsageShaderRead;
        privDesc.storageMode = MTLStorageModePrivate;
        _privateTex[0] = [_sharedDevice newTextureWithDescriptor:privDesc];
        _privateTex[1] = [_sharedDevice newTextureWithDescriptor:privDesc];
        _privateW = latest_width;
        _privateH = latest_height;
        _privateFormat = targetFormat;
    }

    if (!_sharedQueue) {
        _sharedQueue = [_sharedDevice newCommandQueue];
    }

    int writeIdx = 1 - _privateWriteIdx;
    id<MTLTexture> dstTex = _privateTex[writeIdx];

    id<MTLCommandBuffer> cmdBuf = [_sharedQueue commandBuffer];
    id<MTLBlitCommandEncoder> blit = [cmdBuf blitCommandEncoder];
    [blit optimizeContentsForGPUAccess:srcSrgbView];
    [blit copyFromTexture:srcSrgbView
              sourceSlice:0
              sourceLevel:0
             sourceOrigin:MTLOriginMake(0, 0, 0)
               sourceSize:MTLSizeMake(latest_width, latest_height, 1)
                toTexture:dstTex
         destinationSlice:0
         destinationLevel:0
        destinationOrigin:MTLOriginMake(0, 0, 0)];
    [blit endEncoding];
    [cmdBuf commit];
    [cmdBuf waitUntilCompleted];

    _privateWriteIdx = writeIdx;

    uint64_t t_end = mach_absolute_time();
    _prof_call_count++;
    if (cacheHit) _prof_cache_hits++; else _prof_cache_misses++;
    _prof_drain_total_ms += ticks_to_ms(t_drain_end - t_drain_start);
    _prof_tex_create_total_ms += ticks_to_ms(t_end - t_tex_start);
    _prof_drain_msg_total += drain_count;
    _prof_total_ms += ticks_to_ms(t_end - t_start);
    if (_prof_call_count % 120 == 0) {
        NSLog(@"[CefUnity-Prof] calls=%llu hit=%d miss=%d drain_msgs=%d | drain=%.2fms tex+blit=%.2fms total=%.2fms (avg over 120)",
              _prof_call_count, _prof_cache_hits, _prof_cache_misses, _prof_drain_msg_total,
              _prof_drain_total_ms, _prof_tex_create_total_ms, _prof_total_ms);
        _prof_drain_total_ms = _prof_lookup_total_ms = _prof_tex_create_total_ms = _prof_total_ms = 0;
        _prof_drain_msg_total = _prof_cache_hits = _prof_cache_misses = 0;
    }

    *out_width = (int32_t)latest_width;
    *out_height = (int32_t)latest_height;
    *out_format = latest_format;
    return (__bridge_retained void*)dstTex;
}

// ---------------------------------------------------------------------------
// Legacy IOSurfaceLookup (kept for backward compat, broken on macOS 16)
// ---------------------------------------------------------------------------

void* cef_unity_create_metal_texture_objc(
    uint32_t surface_id,
    int32_t width,
    int32_t height,
    uint32_t format)
{
    if (surface_id == 0 || width <= 0 || height <= 0) return NULL;

    if (!_sharedDevice) {
        _sharedDevice = MTLCreateSystemDefaultDevice();
        if (!_sharedDevice) return NULL;
    }

    IOSurfaceRef surface = IOSurfaceLookup(surface_id);
    if (!surface) return NULL;

    MTLPixelFormat pixelFormat = (format == 1)
        ? MTLPixelFormatRGBA8Unorm
        : MTLPixelFormatBGRA8Unorm;

    MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:pixelFormat
                                                                                   width:(NSUInteger)width
                                                                                  height:(NSUInteger)height
                                                                               mipmapped:NO];
    desc.usage = MTLTextureUsageShaderRead | MTLTextureUsagePixelFormatView;
    desc.storageMode = MTLStorageModeShared;

    id<MTLTexture> texture = [_sharedDevice newTextureWithDescriptor:desc
                                                            iosurface:surface
                                                                plane:0];
    CFRelease(surface);
    if (!texture) return NULL;

    return (__bridge_retained void*)texture;
}

void cef_unity_release_metal_texture_objc(void* texture_ptr)
{
    if (!texture_ptr) return;
    id<MTLTexture> texture = (__bridge_transfer id<MTLTexture>)texture_ptr;
    (void)texture;
}
