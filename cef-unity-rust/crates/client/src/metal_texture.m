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

// 直近に受信した IOSurface / そのテクスチャ (キャッシュが retain 済み)。診断用。
static IOSurfaceRef _lastReceivedSurface = NULL;
static id<MTLTexture> _lastReceivedTexture = nil;

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
            _lastReceivedTexture = _surfaceCache[cache_index].srgbView;
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
            _lastReceivedTexture = srgbView;
        }
    }

    *out_width = (int32_t)latest_width;
    *out_height = (int32_t)latest_height;
    *out_format = latest_format;
    return (__bridge_retained void*)srgbView;
}

// GPU 読みティアリング検出 (診断専用)
//
// CPU 読み (IOSurfaceLock) は lock 自体が GPU 同期を行うため、「読んだ瞬間の内容」
// ではなく「完了後の内容」しか見えず、GPU 可視性の破れを観測できない (実測で
// 既知不良構成でも検出ゼロだった)。Unity が実際に行うのは GPU からのサンプルなので、
// ここでは自前の command queue で 1 列を staging buffer へ blit し、その結果を読む。
// サーバー側の blit と我々の読みは別キュー = 順序保証が無いため、未完了の転送先を
// 読めば「複数フレームの混在 (ティアリング)」か「古い内容 (ロールバック)」が現れる。

#define VERIFY_BYTES_PER_ROW 256  // copyFromTexture:toBuffer: の行アライメント要件を満たす

static id<MTLCommandQueue> _verifyQueue = nil;
static id<MTLBuffer> _verifyBuffer = nil;
static NSUInteger _verifyBufferHeight = 0;

/// 直近に受信した IOSurface の 1 列 (中央 x) を GPU で読み出し、縦方向に等間隔な
/// count 個の画素を out_pixels に書く。戻り値は書き込んだ画素数 (0 = 失敗/未受信)。
int cef_unity_verify_last_iosurface_gpu_objc(uint32_t* out_pixels, int32_t count) {
    if (out_pixels == NULL || count <= 0) return 0;
    if (_lastReceivedTexture == nil || _sharedDevice == nil) return 0;

    id<MTLTexture> texture = _lastReceivedTexture;
    NSUInteger width = texture.width;
    NSUInteger height = texture.height;
    if (width == 0 || height == 0) return 0;

    @autoreleasepool {
        if (_verifyQueue == nil) {
            _verifyQueue = [_sharedDevice newCommandQueue];
            if (_verifyQueue == nil) return 0;
        }
        if (_verifyBuffer == nil || _verifyBufferHeight != height) {
            _verifyBuffer = [_sharedDevice newBufferWithLength:height * VERIFY_BYTES_PER_ROW
                                                      options:MTLResourceStorageModeShared];
            if (_verifyBuffer == nil) return 0;
            _verifyBufferHeight = height;
        }

        id<MTLCommandBuffer> commandBuffer = [_verifyQueue commandBuffer];
        if (commandBuffer == nil) return 0;
        id<MTLBlitCommandEncoder> blit = [commandBuffer blitCommandEncoder];
        [blit copyFromTexture:texture
                  sourceSlice:0
                  sourceLevel:0
                 sourceOrigin:(MTLOrigin){width / 2, 0, 0}
                   sourceSize:(MTLSize){1, height, 1}
                     toBuffer:_verifyBuffer
            destinationOffset:0
       destinationBytesPerRow:VERIFY_BYTES_PER_ROW
     destinationBytesPerImage:height * VERIFY_BYTES_PER_ROW];
        [blit endEncoding];
        [commandBuffer commit];
        [commandBuffer waitUntilCompleted];

        const uint8_t* base = (const uint8_t*)_verifyBuffer.contents;
        for (int32_t index = 0; index < count; index++) {
            NSUInteger row = (NSUInteger)((double)index / (double)count * (double)height);
            if (row >= height) row = height - 1;
            out_pixels[index] = *(const uint32_t*)(base + row * VERIFY_BYTES_PER_ROW);
        }
    }
    return count;
}

/// 診断専用 (issue #10): このプロセスが保持している Mach port 名の総数を返す。
/// connect 毎に receive port が増えていくリークを外部ツール無しで観測するため。
int cef_unity_debug_mach_port_count_objc(void) {
    mach_port_name_array_t names = NULL;
    mach_msg_type_number_t name_count = 0;
    mach_port_type_array_t types = NULL;
    mach_msg_type_number_t type_count = 0;
    if (mach_port_names(mach_task_self(), &names, &name_count, &types, &type_count)
            != KERN_SUCCESS) {
        return -1;
    }
    int result = (int)name_count;
    if (names) vm_deallocate(mach_task_self(), (vm_address_t)names,
                             name_count * sizeof(mach_port_name_t));
    if (types) vm_deallocate(mach_task_self(), (vm_address_t)types,
                             type_count * sizeof(mach_port_type_t));
    return result;
}

/// 診断専用 (issue #10): 受信ポートと surface キャッシュの現在値を返す。
/// receive_port が connect 毎に変わり、古いポートが解放されていないことを見るため。
int cef_unity_debug_iosurface_state_objc(uint32_t* out_receive_port, int32_t* out_cache_count) {
    if (out_receive_port) *out_receive_port = (uint32_t)g_receive_port;
    if (out_cache_count) *out_cache_count = _surfaceCacheCount;
    return 1;
}

void cef_unity_release_metal_texture_objc(void* texture_pointer)
{
    if (!texture_pointer) return;
    id<MTLTexture> texture = (__bridge_transfer id<MTLTexture>)texture_pointer;
    (void)texture;
}
