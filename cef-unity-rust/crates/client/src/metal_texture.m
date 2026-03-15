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
// IOSurface テクスチャ + sRGB view キャッシュ (IOSurface が同一なら再利用)
static IOSurfaceRef _cachedSurface = NULL;
static id<MTLTexture> _cachedSrgbView = nil;

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

    // Drain all pending messages, keep only the latest
    IOSurfaceRef latest_surface = NULL;
    uint32_t latest_width = 0, latest_height = 0, latest_format = 0;

    for (;;) {
        // Receive buffer must include space for mach_msg_trailer_t (8 bytes)
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
            0,  // non-blocking
            MACH_PORT_NULL
        );

        if (kr != MACH_MSG_SUCCESS) break;

        // Got a message — extract IOSurface
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

    // IOSurface が前回と同じならキャッシュ済み sRGB view を再利用
    if (latest_surface == _cachedSurface && _cachedSrgbView) {
        CFRelease(latest_surface);
        *out_width = (int32_t)latest_width;
        *out_height = (int32_t)latest_height;
        *out_format = latest_format;
        return (__bridge_retained void*)_cachedSrgbView;
    }

    // IOSurface テクスチャを作成 (PixelFormatView 対応)
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

    // sRGB texture view: 同じメモリを sRGB として再解釈（GPU コピーなし）
    MTLPixelFormat srgbFormat = (latest_format == 1)
        ? MTLPixelFormatRGBA8Unorm_sRGB
        : MTLPixelFormatBGRA8Unorm_sRGB;

    id<MTLTexture> srgbView = [iosTex newTextureViewWithPixelFormat:srgbFormat];
    if (!srgbView) {
        NSLog(@"[CefUnity-Mach] newTextureViewWithPixelFormat failed, falling back to base texture");
        srgbView = iosTex;
    }

    // キャッシュ更新
    if (_cachedSurface) CFRelease(_cachedSurface);
    _cachedSurface = latest_surface;
    CFRetain(_cachedSurface);
    _cachedSrgbView = srgbView;
    CFRelease(latest_surface);

    NSLog(@"[CefUnity-Mach] sRGB view: %ux%u pixelFormat=%lu (base=%lu)",
          latest_width, latest_height,
          (unsigned long)srgbView.pixelFormat, (unsigned long)iosTex.pixelFormat);

    *out_width = (int32_t)latest_width;
    *out_height = (int32_t)latest_height;
    *out_format = latest_format;
    return (__bridge_retained void*)srgbView;
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
