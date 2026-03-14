// Metal/IOSurface bridge for CEF-Unity GPU texture sharing.
// Creates MTLTexture backed by an IOSurface (zero-copy).

#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>

static id<MTLDevice> _sharedDevice = nil;

void* cef_unity_create_metal_texture_objc(
    uint32_t surface_id,
    int32_t width,
    int32_t height,
    uint32_t format)
{
    if (surface_id == 0 || width <= 0 || height <= 0) {
        return NULL;
    }

    if (!_sharedDevice) {
        _sharedDevice = MTLCreateSystemDefaultDevice();
        if (!_sharedDevice) return NULL;
    }
    id<MTLDevice> device = _sharedDevice;

    IOSurfaceRef surface = IOSurfaceLookup(surface_id);
    if (!surface) {
        return NULL;
    }

    MTLPixelFormat pixelFormat = (format == 1)
        ? MTLPixelFormatRGBA8Unorm
        : MTLPixelFormatBGRA8Unorm;

    MTLTextureDescriptor *desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:pixelFormat
                                                                                   width:(NSUInteger)width
                                                                                  height:(NSUInteger)height
                                                                               mipmapped:NO];
    desc.usage = MTLTextureUsageShaderRead;
    desc.storageMode = MTLStorageModeShared;

    id<MTLTexture> texture = [device newTextureWithDescriptor:desc
                                                    iosurface:surface
                                                        plane:0];
    CFRelease(surface);

    if (!texture) {
        return NULL;
    }

    return (__bridge_retained void*)texture;
}

void cef_unity_release_metal_texture_objc(void* texture_ptr)
{
    if (!texture_ptr) return;
    // Transfer back to ARC for release
    id<MTLTexture> texture = (__bridge_transfer id<MTLTexture>)texture_ptr;
    (void)texture; // ARC releases when scope ends
}
