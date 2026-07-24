// AudioUnit (DefaultOutput) 出力シム。
// Unity の FMOD ミキサを迂回して CoreAudio に直結する低遅延経路 (CRI 方式)。
// AudioUnit は C API なので Obj-C 不要。metal_texture.m と同じ cc ビルドパターン。
//
// スレッド/ライフサイクル契約:
// - pull は CoreAudio render callback スレッドから呼ばれる (io_frames ごと ≈ 2.9ms@128)。
// - audio_unit_output_stop は detached フラグ → Stop → 実行中 callback の排水待ち、の順で
//   同期停止する。返った後 pull は二度と呼ばれないので context を安全に解放できる。
#include <AudioUnit/AudioUnit.h>
#include <CoreAudio/CoreAudio.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

typedef int32_t (*audio_unit_pull_fn)(void* context, float* out, int32_t frames);

typedef struct {
    AudioUnit unit;
    audio_unit_pull_fn pull;
    void* context;
    _Atomic float volume;
    atomic_int detached;
    atomic_int active;
} audio_unit_output_t;

static OSStatus audio_unit_render(void* reference, AudioUnitRenderActionFlags* flags,
                          const AudioTimeStamp* timestamp, UInt32 bus,
                          UInt32 frames, AudioBufferList* io) {
    (void)flags; (void)timestamp; (void)bus;
    audio_unit_output_t* output = (audio_unit_output_t*)reference;
    float* out = (float*)io->mBuffers[0].mData;
    UInt32 samples = frames * io->mBuffers[0].mNumberChannels;

    atomic_fetch_add_explicit(&output->active, 1, memory_order_acquire);
    if (atomic_load_explicit(&output->detached, memory_order_acquire)) {
        memset(out, 0, samples * sizeof(float));
    } else {
        output->pull(output->context, out, (int32_t)frames);
        float volume = atomic_load_explicit(&output->volume, memory_order_relaxed);
        if (volume != 1.0f) {
            for (UInt32 sample_index = 0; sample_index < samples; sample_index++) out[sample_index] *= volume;
        }
    }
    atomic_fetch_sub_explicit(&output->active, 1, memory_order_release);
    return noErr;
}

void* audio_unit_output_start(double source_rate, int32_t channels, int32_t io_frames,
                      audio_unit_pull_fn pull, void* context) {
    AudioComponentDescription descriptor;
    memset(&descriptor, 0, sizeof(descriptor));
    descriptor.componentType = kAudioUnitType_Output;
    // DefaultOutput はデフォルトデバイスの切替に自動追従する。
    descriptor.componentSubType = kAudioUnitSubType_DefaultOutput;
    descriptor.componentManufacturer = kAudioUnitManufacturer_Apple;
    AudioComponent component = AudioComponentFindNext(NULL, &descriptor);
    if (!component) return NULL;

    audio_unit_output_t* output = (audio_unit_output_t*)calloc(1, sizeof(audio_unit_output_t));
    if (!output) return NULL;
    output->pull = pull;
    output->context = context;
    atomic_store(&output->volume, 1.0f);

    if (AudioComponentInstanceNew(component, &output->unit) != noErr) {
        free(output);
        return NULL;
    }

    // 入力スコープに src フォーマットを設定 → AU 内蔵コンバータがデバイスレートへ
    // 変換する (手動 SRC 不要。残るは ppm ドリフトのみで、それは steering が吸収)。
    AudioStreamBasicDescription format;
    memset(&format, 0, sizeof(format));
    format.mSampleRate = source_rate;
    format.mFormatID = kAudioFormatLinearPCM;
    format.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    format.mFramesPerPacket = 1;
    format.mChannelsPerFrame = (UInt32)channels;
    format.mBitsPerChannel = 32;
    format.mBytesPerFrame = (UInt32)channels * 4;
    format.mBytesPerPacket = (UInt32)channels * 4;
    if (AudioUnitSetProperty(output->unit, kAudioUnitProperty_StreamFormat,
                             kAudioUnitScope_Input, 0, &format, sizeof(format)) != noErr) {
        AudioComponentInstanceDispose(output->unit);
        free(output);
        return NULL;
    }

    // IO バッファフレーム数。デバイス共有の設定なので他アプリの callback 周期にも
    // 影響する。失敗してもデバイス既定サイズで動くので続行。
    UInt32 io_size = (UInt32)io_frames;
    AudioUnitSetProperty(output->unit, kAudioDevicePropertyBufferFrameSize,
                         kAudioUnitScope_Global, 0, &io_size, sizeof(io_size));

    AURenderCallbackStruct callback;
    callback.inputProc = audio_unit_render;
    callback.inputProcRefCon = output;
    if (AudioUnitSetProperty(output->unit, kAudioUnitProperty_SetRenderCallback,
                             kAudioUnitScope_Input, 0, &callback, sizeof(callback)) != noErr ||
        AudioUnitInitialize(output->unit) != noErr) {
        AudioComponentInstanceDispose(output->unit);
        free(output);
        return NULL;
    }
    if (AudioOutputUnitStart(output->unit) != noErr) {
        AudioUnitUninitialize(output->unit);
        AudioComponentInstanceDispose(output->unit);
        free(output);
        return NULL;
    }
    return output;
}

void audio_unit_output_stop(void* handle) {
    audio_unit_output_t* output = (audio_unit_output_t*)handle;
    if (!output) return;
    // DetachAndWait: 以降の callback は pull せず無音 → Stop → 実行中 callback の排水待ち。
    atomic_store_explicit(&output->detached, 1, memory_order_release);
    AudioOutputUnitStop(output->unit);
    while (atomic_load_explicit(&output->active, memory_order_acquire) != 0) {
        // callback は µs オーダーなので実質即時に抜ける。
    }
    AudioUnitUninitialize(output->unit);
    AudioComponentInstanceDispose(output->unit);
    free(output);
}

void audio_unit_output_set_volume(void* handle, float volume) {
    audio_unit_output_t* output = (audio_unit_output_t*)handle;
    if (!output) return;
    atomic_store_explicit(&output->volume, volume, memory_order_relaxed);
}
