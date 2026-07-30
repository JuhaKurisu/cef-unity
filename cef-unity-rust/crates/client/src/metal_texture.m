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
static id<MTLDevice> _sharedDevice = nil;

// IOSurface テクスチャ + sRGB view キャッシュ (IOSurfaceID で比較、マルチエントリ)
#define IOSURFACE_CACHE_SIZE 4
static struct {
    IOSurfaceID surfaceID;
    IOSurfaceRef surface;
    id<MTLTexture> srgbView;
} _surfaceCache[IOSURFACE_CACHE_SIZE];
static int _surfaceCacheCount = 0;

// ---------------------------------------------------------------------------
// Mach port IOSurface client
// ---------------------------------------------------------------------------

// Must match server's iosurface_message_t layout
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t surface_port;
    uint32_t width;
    uint32_t height;
    uint32_t format;
} iosurface_message_t;

// Subscribe message (client → server)
typedef struct {
    mach_msg_header_t header;
    mach_msg_body_t body;
    mach_msg_port_descriptor_t client_port;
} subscribe_message_t;

static mach_port_t g_receive_port = MACH_PORT_NULL;

// 直近に受信した IOSurface (キャッシュが retain 済み)。診断用の画素サンプルに使う。
static IOSurfaceRef _lastReceivedSurface = NULL;

/// Connect to the server's Mach IOSurface service and send subscription.
/// Returns 0 on success, negative on error.
int mach_iosurface_client_connect(const char* service_name) {
    kern_return_t kern_return_value;
    mach_port_t server_port;

    kern_return_value = bootstrap_look_up(bootstrap_port, service_name, &server_port);
    if (kern_return_value != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] bootstrap_look_up('%s') failed: %s", service_name, mach_error_string(kern_return_value));
        return -1;
    }

    // Create our receive port
    kern_return_value = mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &g_receive_port);
    if (kern_return_value != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] mach_port_allocate failed: %s", mach_error_string(kern_return_value));
        mach_port_deallocate(mach_task_self(), server_port);
        return -2;
    }

    // Send subscription message with our port (as a send right)
    subscribe_message_t message;
    __builtin_memset(&message, 0, sizeof(message));

    message.header.msgh_bits = MACH_MSGH_BITS_COMPLEX |
                           MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    message.header.msgh_size = sizeof(message);
    message.header.msgh_remote_port = server_port;
    message.header.msgh_local_port = MACH_PORT_NULL;
    message.header.msgh_id = 0x53554253;  // 'SUBS'

    message.body.msgh_descriptor_count = 1;

    message.client_port.name = g_receive_port;
    message.client_port.disposition = MACH_MSG_TYPE_MAKE_SEND;
    message.client_port.type = MACH_MSG_PORT_DESCRIPTOR;

    kern_return_value = mach_msg(
        &message.header,
        MACH_SEND_MSG | MACH_SEND_TIMEOUT,
        sizeof(message),
        0,
        MACH_PORT_NULL,
        5000,  // 5s timeout
        MACH_PORT_NULL
    );

    mach_port_deallocate(mach_task_self(), server_port);

    if (kern_return_value != KERN_SUCCESS) {
        NSLog(@"[CefUnity-Mach] subscribe send failed: %s", mach_error_string(kern_return_value));
        return -3;
    }

    NSLog(@"[CefUnity-Mach] connected to '%s', receive port=%u", service_name, g_receive_port);
    return 0;
}

/// Non-blocking receive of IOSurface from Mach port channel.
/// On success, returns an MTLTexture pointer (retained, caller must release).
/// On no message or error, returns NULL.
///
/// Server-side pool copy ensures the IOSurface content is stable (won't be
/// overwritten by CEF). We just create a sRGB texture view and return it
/// directly — no client-side GPU blit or waitUntilCompleted needed.
void* mach_iosurface_receive_texture(int32_t* out_width, int32_t* out_height, uint32_t* out_format) {
    if (g_receive_port == MACH_PORT_NULL) return NULL;

    // Drain all pending messages, keep only the latest
    IOSurfaceRef latest_surface = NULL;
    uint32_t latest_width = 0, latest_height = 0, latest_format = 0;
    for (;;) {
        struct {
            iosurface_message_t message;
            mach_msg_trailer_t trailer;
        } receive_buffer;
        __builtin_memset(&receive_buffer, 0, sizeof(receive_buffer));

        kern_return_t kern_return_value = mach_msg(
            &receive_buffer.message.header,
            MACH_RCV_MSG | MACH_RCV_TIMEOUT,
            0,
            sizeof(receive_buffer),
            g_receive_port,
            0,
            MACH_PORT_NULL
        );

        if (kern_return_value != MACH_MSG_SUCCESS) break;

        mach_port_t surface_port = receive_buffer.message.surface_port.name;
        IOSurfaceRef surface = IOSurfaceLookupFromMachPort(surface_port);
        mach_port_deallocate(mach_task_self(), surface_port);

        if (surface) {
            if (latest_surface) CFRelease(latest_surface);
            latest_surface = surface;
            latest_width = receive_buffer.message.width;
            latest_height = receive_buffer.message.height;
            latest_format = receive_buffer.message.format;
        }
    }

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

    // マルチエントリキャッシュで IOSurfaceID を検索
    IOSurfaceID latestID = IOSurfaceGetID(latest_surface);
    id<MTLTexture> srgbView = nil;

    for (int cache_index = 0; cache_index < _surfaceCacheCount; cache_index++) {
        if (_surfaceCache[cache_index].surfaceID == latestID && _surfaceCache[cache_index].srgbView) {
            CFRelease(latest_surface);
            srgbView = _surfaceCache[cache_index].srgbView;
            _lastReceivedSurface = _surfaceCache[cache_index].surface;
            break;
        }
    }

    if (!srgbView) {
        @autoreleasepool {
            // キャッシュミス: IOSurface テクスチャを作成
            MTLPixelFormat iosurfaceFormat = (latest_format == 1)
                ? MTLPixelFormatRGBA8Unorm
                : MTLPixelFormatBGRA8Unorm;

            MTLTextureDescriptor *descriptor = [MTLTextureDescriptor
                texture2DDescriptorWithPixelFormat:iosurfaceFormat
                                             width:(NSUInteger)latest_width
                                            height:(NSUInteger)latest_height
                                         mipmapped:NO];
            descriptor.usage = MTLTextureUsageShaderRead | MTLTextureUsagePixelFormatView;
            descriptor.storageMode = MTLStorageModeShared;

            id<MTLTexture> iosurfaceTexture = [_sharedDevice newTextureWithDescriptor:descriptor
                                                                  iosurface:latest_surface
                                                                      plane:0];
            if (!iosurfaceTexture) {
                NSLog(@"[CefUnity-Mach] newTextureWithDescriptor:iosurface: returned nil");
                CFRelease(latest_surface);
                return NULL;
            }

            MTLPixelFormat srgbFormat = (latest_format == 1)
                ? MTLPixelFormatRGBA8Unorm_sRGB
                : MTLPixelFormatBGRA8Unorm_sRGB;

            srgbView = [iosurfaceTexture newTextureViewWithPixelFormat:srgbFormat];
            if (!srgbView) srgbView = iosurfaceTexture;

            // キャッシュに追加
            int slot;
            if (_surfaceCacheCount < IOSURFACE_CACHE_SIZE) {
                slot = _surfaceCacheCount++;
            } else {
                if (_surfaceCache[0].surface) CFRelease(_surfaceCache[0].surface);
                for (int cache_index = 0; cache_index < IOSURFACE_CACHE_SIZE - 1; cache_index++)
                    _surfaceCache[cache_index] = _surfaceCache[cache_index + 1];
                slot = IOSURFACE_CACHE_SIZE - 1;
            }
            _surfaceCache[slot].surfaceID = latestID;
            _surfaceCache[slot].surface = latest_surface;
            _surfaceCache[slot].srgbView = srgbView;
            _lastReceivedSurface = latest_surface;
        }
    }

    *out_width = (int32_t)latest_width;
    *out_height = (int32_t)latest_height;
    *out_format = latest_format;
    return (__bridge_retained void*)srgbView;
}

/// 直近に受信した IOSurface から、縦方向に等間隔な行の中央画素を count 個サンプルする
/// (診断専用)。全画面が単色のページでは全て同じ値になるはずで、値が割れていれば
/// ティアリング = 転送先が blit 未完了 or src が上書きされた証拠になる。
/// 戻り値: 書き込んだ画素数。0 は未受信 or lock 失敗。
int cef_unity_sample_iosurface_pixels_objc(uint32_t* out_pixels, int32_t count) {
    if (out_pixels == NULL || count <= 0 || _lastReceivedSurface == NULL) return 0;

    IOSurfaceRef surface = _lastReceivedSurface;
    if (IOSurfaceLock(surface, kIOSurfaceLockReadOnly, NULL) != kIOReturnSuccess) return 0;

    uint8_t* base = (uint8_t*)IOSurfaceGetBaseAddress(surface);
    size_t bytes_per_row = IOSurfaceGetBytesPerRow(surface);
    size_t width = IOSurfaceGetWidth(surface);
    size_t height = IOSurfaceGetHeight(surface);
    if (base == NULL || width == 0 || height == 0) {
        IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
        return 0;
    }

    for (int32_t index = 0; index < count; index++) {
        size_t row = (size_t)((double)index / (double)count * (double)height);
        if (row >= height) row = height - 1;
        uint32_t* pixels_in_row = (uint32_t*)(base + row * bytes_per_row);
        out_pixels[index] = pixels_in_row[width / 2];
    }

    IOSurfaceUnlock(surface, kIOSurfaceLockReadOnly, NULL);
    return count;
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
    // Rust スレッドから呼ばれる (pool なし) — autoreleased な descriptor を
    // 蓄積させないため必ず pool で囲む。
    @autoreleasepool {
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

        MTLTextureDescriptor *descriptor = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:pixelFormat
                                                                                       width:(NSUInteger)width
                                                                                      height:(NSUInteger)height
                                                                                   mipmapped:NO];
        descriptor.usage = MTLTextureUsageShaderRead | MTLTextureUsagePixelFormatView;
        descriptor.storageMode = MTLStorageModeShared;

        id<MTLTexture> texture = [_sharedDevice newTextureWithDescriptor:descriptor
                                                                iosurface:surface
                                                                    plane:0];
        CFRelease(surface);
        if (!texture) return NULL;

        return (__bridge_retained void*)texture;
    }
}

void cef_unity_release_metal_texture_objc(void* texture_pointer)
{
    if (!texture_pointer) return;
    id<MTLTexture> texture = (__bridge_transfer id<MTLTexture>)texture_pointer;
    (void)texture;
}
