// AudioUnit (DefaultOutput) 出力シム。
// Unity の FMOD ミキサを迂回して CoreAudio に直結する低遅延経路 (CRI 方式)。
// AudioUnit は C API なので Obj-C 不要。metal_texture.m と同じ cc ビルドパターン。
//
// スレッド/ライフサイクル契約:
// - pull は CoreAudio render callback スレッドから呼ばれる (io_frames ごと ≈ 2.9ms@128)。
// - au_output_stop は detached フラグ → Stop → 実行中 callback の排水待ち、の順で
//   同期停止する。返った後 pull は二度と呼ばれないので ctx を安全に解放できる。
#include <AudioUnit/AudioUnit.h>
#include <CoreAudio/CoreAudio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t (*au_pull_fn)(void* ctx, float* out, int32_t frames);

typedef struct {
    AudioUnit unit;
    au_pull_fn pull;
    void* ctx;
    _Atomic float volume;
    atomic_int detached;
    atomic_int active;
} au_output_t;

static OSStatus au_render(void* ref, AudioUnitRenderActionFlags* flags,
                          const AudioTimeStamp* ts, UInt32 bus,
                          UInt32 frames, AudioBufferList* io) {
    (void)flags; (void)ts; (void)bus;
    au_output_t* h = (au_output_t*)ref;
    float* out = (float*)io->mBuffers[0].mData;
    UInt32 samples = frames * io->mBuffers[0].mNumberChannels;

    atomic_fetch_add_explicit(&h->active, 1, memory_order_acquire);
    if (atomic_load_explicit(&h->detached, memory_order_acquire)) {
        memset(out, 0, samples * sizeof(float));
    } else {
        h->pull(h->ctx, out, (int32_t)frames);
        float v = atomic_load_explicit(&h->volume, memory_order_relaxed);
        if (v != 1.0f) {
            for (UInt32 i = 0; i < samples; i++) out[i] *= v;
        }
    }
    atomic_fetch_sub_explicit(&h->active, 1, memory_order_release);
    return noErr;
}

void* au_output_start(double src_rate, int32_t channels, int32_t io_frames,
                      au_pull_fn pull, void* ctx) {
    AudioComponentDescription desc;
    memset(&desc, 0, sizeof(desc));
    desc.componentType = kAudioUnitType_Output;
    // DefaultOutput はデフォルトデバイスの切替に自動追従する。
    desc.componentSubType = kAudioUnitSubType_DefaultOutput;
    desc.componentManufacturer = kAudioUnitManufacturer_Apple;
    AudioComponent comp = AudioComponentFindNext(NULL, &desc);
    if (!comp) return NULL;

    au_output_t* h = (au_output_t*)calloc(1, sizeof(au_output_t));
    if (!h) return NULL;
    h->pull = pull;
    h->ctx = ctx;
    atomic_store(&h->volume, 1.0f);

    if (AudioComponentInstanceNew(comp, &h->unit) != noErr) {
        free(h);
        return NULL;
    }

    // 入力スコープに src フォーマットを設定 → AU 内蔵コンバータがデバイスレートへ
    // 変換する (手動 SRC 不要。残るは ppm ドリフトのみで、それは steering が吸収)。
    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = src_rate;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    fmt.mFramesPerPacket = 1;
    fmt.mChannelsPerFrame = (UInt32)channels;
    fmt.mBitsPerChannel = 32;
    fmt.mBytesPerFrame = (UInt32)channels * 4;
    fmt.mBytesPerPacket = (UInt32)channels * 4;
    if (AudioUnitSetProperty(h->unit, kAudioUnitProperty_StreamFormat,
                             kAudioUnitScope_Input, 0, &fmt, sizeof(fmt)) != noErr) {
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }

    // IO バッファフレーム数。デバイス共有の設定なので他アプリの callback 周期にも
    // 影響する。失敗してもデバイス既定サイズで動くので続行。
    UInt32 io_size = (UInt32)io_frames;
    AudioUnitSetProperty(h->unit, kAudioDevicePropertyBufferFrameSize,
                         kAudioUnitScope_Global, 0, &io_size, sizeof(io_size));

    AURenderCallbackStruct cb;
    cb.inputProc = au_render;
    cb.inputProcRefCon = h;
    if (AudioUnitSetProperty(h->unit, kAudioUnitProperty_SetRenderCallback,
                             kAudioUnitScope_Input, 0, &cb, sizeof(cb)) != noErr ||
        AudioUnitInitialize(h->unit) != noErr) {
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }
    if (AudioOutputUnitStart(h->unit) != noErr) {
        AudioUnitUninitialize(h->unit);
        AudioComponentInstanceDispose(h->unit);
        free(h);
        return NULL;
    }
    return h;
}

void au_output_stop(void* handle) {
    au_output_t* h = (au_output_t*)handle;
    if (!h) return;
    // DetachAndWait: 以降の callback は pull せず無音 → Stop → 実行中 callback の排水待ち。
    atomic_store_explicit(&h->detached, 1, memory_order_release);
    AudioOutputUnitStop(h->unit);
    while (atomic_load_explicit(&h->active, memory_order_acquire) != 0) {
        // callback は µs オーダーなので実質即時に抜ける。
    }
    AudioUnitUninitialize(h->unit);
    AudioComponentInstanceDispose(h->unit);
    free(h);
}

void au_output_set_volume(void* handle, float v) {
    au_output_t* h = (au_output_t*)handle;
    if (!h) return;
    atomic_store_explicit(&h->volume, v, memory_order_relaxed);
}
