// issue #7 (on_accelerated_paint の同期 GPU コピー完了待ちが CEF pump を止める) の
// 再現用ツール。Unity も CEF も使わず、iosurface_pool.m と同じ形の
// 「IOSurface 間 Metal blit + waitUntilCompleted」の所要時間を測り、
// 同時に外部 GPU 競合を作る側にもなる。
//
// ビルド:
//   clang -fobjc-arc -O2 gpu-contention-hog.m \
//       -framework Metal -framework IOSurface -framework Foundation -o gpu-contention-hog
//
// 使い方:
//   ./gpu-contention-hog blit         … 1920x1080 の blit + 完了待ちを 600 回測り分布を出す
//   ./gpu-contention-hog hog [blits]  … GPU を占有し続ける (競合を作る側、既定 64 blit/cmdbuf)
//
// Harness と組み合わせた再現手順 (macOS):
//   1) ./gpu-contention-hog hog を 2〜6 本バックグラウンドで起動
//   2) CefUnity.Harness paint-statistics 12 1920 1080 を実行
//   3) サーバーログの STATISTICS 行 (pump_ticks / copy_wait_total) と
//      クライアント側の received / max_gap を突き合わせる
//   CPU 飽和も加えると pump_ticks の落ち込みがさらに大きくなる (待機スレッドの追い出し)。
#import <Metal/Metal.h>
#import <IOSurface/IOSurface.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <mach/mach_time.h>

static IOSurfaceRef make_surface(uint32_t width, uint32_t height) {
    NSDictionary *props = @{
        (id)kIOSurfaceWidth: @(width),
        (id)kIOSurfaceHeight: @(height),
        (id)kIOSurfaceBytesPerElement: @(4),
        (id)kIOSurfacePixelFormat: @((uint32_t)'BGRA'),
    };
    return IOSurfaceCreate((__bridge CFDictionaryRef)props);
}

static id<MTLTexture> tex_for(id<MTLDevice> device, IOSurfaceRef surface,
                              uint32_t width, uint32_t height, MTLTextureUsage usage) {
    MTLTextureDescriptor *desc =
        [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm
                                                          width:width height:height mipmapped:NO];
    desc.storageMode = MTLStorageModeShared;
    desc.usage = usage;
    return [device newTextureWithDescriptor:desc iosurface:surface plane:0];
}

static double milliseconds_since(uint64_t start, mach_timebase_info_data_t timebase) {
    uint64_t elapsed = mach_absolute_time() - start;
    return (double)elapsed * timebase.numer / timebase.denom / 1.0e6;
}

static int compare_double(const void *left, const void *right) {
    double a = *(const double *)left, b = *(const double *)right;
    return (a > b) - (a < b);
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "blit";
    mach_timebase_info_data_t timebase;
    mach_timebase_info(&timebase);

    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    id<MTLCommandQueue> queue = [device newCommandQueue];
    const uint32_t width = 1920, height = 1080;

    if (strcmp(mode, "hog") == 0) {
        // 大きな blit を 1 コマンドバッファに詰めて GPU キューを占有し続ける。
        // 1 コマンドバッファの実行時間がそのまま「他プロセスが割り込めない時間」になる。
        const int blits_per_command_buffer = argc > 2 ? atoi(argv[2]) : 64;
        IOSurfaceRef a = make_surface(width * 2, height * 2);
        IOSurfaceRef b = make_surface(width * 2, height * 2);
        id<MTLTexture> source = tex_for(device, a, width * 2, height * 2, MTLTextureUsageShaderRead);
        id<MTLTexture> destination = tex_for(device, b, width * 2, height * 2, MTLTextureUsageShaderWrite);
        for (;;) {
            @autoreleasepool {
                id<MTLCommandBuffer> buffer = [queue commandBuffer];
                for (int repeat = 0; repeat < blits_per_command_buffer; repeat++) {
                    id<MTLBlitCommandEncoder> blit = [buffer blitCommandEncoder];
                    [blit copyFromTexture:source sourceSlice:0 sourceLevel:0
                             sourceOrigin:(MTLOrigin){0,0,0}
                               sourceSize:(MTLSize){width * 2, height * 2, 1}
                                toTexture:destination destinationSlice:0 destinationLevel:0
                        destinationOrigin:(MTLOrigin){0,0,0}];
                    [blit endEncoding];
                }
                [buffer commit];
                [buffer waitUntilCompleted];
            }
        }
    }

    IOSurfaceRef source_surface = make_surface(width, height);
    IOSurfaceRef destination_surface = make_surface(width, height);
    id<MTLTexture> source = tex_for(device, source_surface, width, height, MTLTextureUsageShaderRead);
    id<MTLTexture> destination = tex_for(device, destination_surface, width, height, MTLTextureUsageShaderWrite);

    const int frame_count = 600;
    double *samples = calloc(frame_count, sizeof(double));
    for (int frame_index = 0; frame_index < frame_count; frame_index++) {
        @autoreleasepool {
            uint64_t start = mach_absolute_time();
            id<MTLCommandBuffer> buffer = [queue commandBuffer];
            id<MTLBlitCommandEncoder> blit = [buffer blitCommandEncoder];
            [blit copyFromTexture:source sourceSlice:0 sourceLevel:0
                     sourceOrigin:(MTLOrigin){0,0,0}
                       sourceSize:(MTLSize){width, height, 1}
                        toTexture:destination destinationSlice:0 destinationLevel:0
                destinationOrigin:(MTLOrigin){0,0,0}];
            [blit endEncoding];
            [buffer commit];
            [buffer waitUntilCompleted];
            samples[frame_index] = milliseconds_since(start, timebase);
        }
        usleep(16000); // 60fps 相当の間隔
    }

    qsort(samples, frame_count, sizeof(double), compare_double);
    double sum = 0;
    for (int index = 0; index < frame_count; index++) sum += samples[index];
    printf("n=%d mean=%.3fms median=%.3fms p95=%.3fms p99=%.3fms max=%.3fms\n",
           frame_count, sum / frame_count, samples[frame_count / 2],
           samples[(int)(frame_count * 0.95)], samples[(int)(frame_count * 0.99)],
           samples[frame_count - 1]);
    return 0;
}
