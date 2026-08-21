// OpenAY Mic — audio capture source abstraction.
//
// An IAudioSource captures mono 48 kHz int16 audio and delivers it through
// the callback registered with SetCallback(). The engine calls Start()/Stop()
// from a normal (non-RT) thread; implementations MUST call Deliver() from
// their audio/real-time thread.
//
// Hard-RT contract (Phase 3 plan): the whole path
//   source RT thread -> callback -> engine ring push
// must be lock-free — no I/O, no malloc, no mutex, no logging below the
// callback. The engine wires the callback to CapturePipeline::OnAudio, which
// is a pure atomic ring write; keep it that way. Deliver() itself is a direct
// std::function call (pre-registered, no locking), so it is safe on the RT
// thread as long as the registered sink is too.
#ifndef OPENAY_AUDIO_SOURCE_H
#define OPENAY_AUDIO_SOURCE_H

#include <cstddef>
#include <cstdint>
#include <functional>
#include <string>
#include <utility>

namespace openay {

class IAudioSource {
public:
    // `frames` = number of mono int16 samples (10 ms @ 48 kHz = 480).
    using AudioCallback = std::function<void(const int16_t* samples, size_t frames)>;

    virtual ~IAudioSource() = default;

    // Bring the capture device/stream up. Returns false on failure (the
    // implementation records/logs the reason). Not an RT call.
    virtual bool Start() = 0;

    // Tear the capture device/stream down. Must return promptly and must NOT
    // be called from the audio/RT thread itself. After Stop() returns the
    // implementation guarantees no further Deliver() invocations.
    virtual void Stop() = 0;

    // The engine registers the capture sink (the pipeline's non-blocking
    // push) exactly once, before Start(). Not synchronized: call it before
    // the source starts, from the control thread.
    void SetCallback(AudioCallback cb) { callback_ = std::move(cb); }

    // Platform-specific stats hooks; base implementations are the host/sim
    // defaults. All safe to call from the stats path (never the RT thread).
    virtual std::string sharing_mode() const { return "exclusive"; }
    virtual uint64_t xruns() const { return 0; }
    virtual std::string error_string() const { return ""; }

protected:
    // Implementation hook: call from the audio/RT thread for every captured
    // frame. Non-blocking and must not throw.
    void Deliver(const int16_t* samples, size_t frames) {
        if (callback_) callback_(samples, frames);
    }

private:
    AudioCallback callback_;
};

}  // namespace openay

#endif  // OPENAY_AUDIO_SOURCE_H
