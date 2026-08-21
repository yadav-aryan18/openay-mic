// OpenAY Mic — Oboe (Android) microphone source.
//
// Compiled ONLY when CMAKE_SYSTEM_NAME STREQUAL "Android" (target openaymic);
// the header is additionally guarded so an accidental host include is a
// no-op. Wraps oboe::AudioStreamBuilder into IAudioSource:
//
//   direction Input, sample rate 48000, 1 channel, AudioFormat::I16,
//   PerformanceMode::LowLatency, SharingMode::Exclusive (falls back to Shared
//   when the exclusive open fails), DataCallback (this), ErrorCallback (this).
//
// Hard-RT contract: onAudioReady() only forwards the captured samples via
// Deliver() (the engine's lock-free ring push) and returns
// DataCallbackResult::Continue. No I/O, no malloc, no mutex, no logging on
// that thread. Everything else — open/close, sharing-mode fallback, xrun and
// error bookkeeping — happens off the RT thread.
//
// Note: Oboe may negotiate a sample rate different from the requested one on
// some devices; the pipeline assumes 48 kHz mono. Devices that cannot provide
// 48 kHz input are out of scope for the current protocol.
#ifndef OPENAY_OBOE_SOURCE_H
#define OPENAY_OBOE_SOURCE_H

#if defined(__ANDROID__)

#include <oboe/Oboe.h>

#include <atomic>
#include <mutex>
#include <string>

#include "openay/audio_source.h"

namespace openay {

class OboeSource : public IAudioSource,
                   public oboe::AudioStreamDataCallback,
                   public oboe::AudioStreamErrorCallback {
public:
    OboeSource() = default;
    ~OboeSource() override { Stop(); }

    // Builds and opens the input stream: Exclusive first, retry Shared on
    // failure. Records which mode is active (sharing_mode()).
    bool Start() override;
    // Stops and closes the stream; safe to call when not started.
    void Stop() override;

    // "exclusive" | "shared" | "" ("" if never opened successfully).
    std::string sharing_mode() const override;
    // AudioStream::getXRunCount() — read from the stats path, never from the
    // audio callback. 0 when no stream is open.
    uint64_t xruns() const override;
    // Last onErrorAfterClose text ("" when no stream error occurred).
    std::string error_string() const override;

    // oboe::AudioStreamDataCallback — runs on the audio RT thread.
    // Push-only: forward samples and continue.
    oboe::DataCallbackResult onAudioReady(oboe::AudioStream* stream,
                                          void* audio_data,
                                          int32_t num_frames) override;

    // oboe::AudioStreamErrorCallback — runs on Oboe's internal thread (not
    // the RT thread). Records the error for stats; the stream is already
    // closed when this fires.
    void onErrorAfterClose(oboe::AudioStream* stream,
                           oboe::Result error) override;

private:
    // Protects stream_ / error text; never taken on the RT thread. Mutable:
    // the const stats accessors (sharing_mode/xruns/error_string) lock it.
    mutable std::mutex mu_;
    oboe::AudioStream* stream_ = nullptr;
    std::atomic<bool> stream_error_{false};
    std::string error_text_;
    std::string sharing_mode_;
};

}  // namespace openay

#endif  // __ANDROID__
#endif  // OPENAY_OBOE_SOURCE_H
