// Oboe input-stream implementation (Android only; see oboe_source.h).
#include "openay/oboe_source.h"

namespace openay {

bool OboeSource::Start() {
    std::lock_guard<std::mutex> lk(mu_);
    if (stream_) return true;  // already running

    oboe::AudioStreamBuilder builder;
    builder.setDirection(oboe::Direction::Input);
    builder.setSampleRate(48000);
    builder.setChannelCount(1);
    builder.setFormat(oboe::AudioFormat::I16);
    builder.setPerformanceMode(oboe::PerformanceMode::LowLatency);
    builder.setSharingMode(oboe::SharingMode::Exclusive);
    builder.setDataCallback(this);
    builder.setErrorCallback(this);
    // Allocate an audio session id so the app can attach platform voice
    // effects (echo cancellation / noise suppression) to this stream. On
    // API < 28 the request is ignored and the stream still opens.
    builder.setSessionId(allocate_session_ ? oboe::SessionId::Allocate
                                           : oboe::SessionId::None);

    stream_error_.store(false, std::memory_order_relaxed);
    error_text_.clear();
    session_id_ = 0;

    oboe::Result rc = builder.openStream(&stream_);
    if (rc != oboe::Result::OK) {
        // Exclusive input is frequently unavailable (e.g. when another app
        // owns the mic); retry with Shared before giving up.
        builder.setSharingMode(oboe::SharingMode::Shared);
        rc = builder.openStream(&stream_);
        if (rc != oboe::Result::OK) {
            stream_ = nullptr;
            error_text_ = std::string("open failed: ") + oboe::convertToText(rc);
            return false;
        }
        sharing_mode_ = "shared";
    } else {
        sharing_mode_ = "exclusive";
    }

    // getSessionId() returns the raw SessionId: a positive number once the
    // framework allocated one, or None/-1. Only a real id is reported.
    const int sid = static_cast<int>(stream_->getSessionId());
    session_id_ = sid > 0 ? sid : 0;

    rc = stream_->requestStart();
    if (rc != oboe::Result::OK) {
        error_text_ =
            std::string("requestStart failed: ") + oboe::convertToText(rc);
        stream_->close();
        stream_ = nullptr;
        session_id_ = 0;
        return false;
    }
    return true;
}

void OboeSource::Stop() {
    std::lock_guard<std::mutex> lk(mu_);
    if (!stream_) return;
    oboe::AudioStream* s = stream_;
    stream_ = nullptr;  // close() below may run its own teardown
    s->close();         // also stops the stream
    session_id_ = 0;
}

std::string OboeSource::sharing_mode() const {
    std::lock_guard<std::mutex> lk(mu_);
    return sharing_mode_;
}

uint64_t OboeSource::xruns() const {
    std::lock_guard<std::mutex> lk(mu_);
    if (!stream_) return 0;
    // oboe 1.9: getXRunCount() returns ResultWithValue<int32_t>; 0 on error.
    const auto xr = stream_->getXRunCount();
    if (!xr) return 0;
    const int32_t count = xr.value();
    return count > 0 ? static_cast<uint64_t>(count) : 0;
}

std::string OboeSource::error_string() const {
    std::lock_guard<std::mutex> lk(mu_);
    return error_text_;
}

int OboeSource::audio_session_id() const {
    std::lock_guard<std::mutex> lk(mu_);
    return session_id_;
}

oboe::DataCallbackResult OboeSource::onAudioReady(oboe::AudioStream* stream,
                                                  void* audio_data,
                                                  int32_t num_frames) {
    // Hard-RT thread: push-only. No I/O, no malloc, no mutex, no logging.
    (void)stream;
    Deliver(static_cast<const int16_t*>(audio_data),
            static_cast<size_t>(num_frames));
    return oboe::DataCallbackResult::Continue;
}

void OboeSource::onErrorAfterClose(oboe::AudioStream* stream,
                                   oboe::Result error) {
    (void)stream;
    std::lock_guard<std::mutex> lk(mu_);
    stream_error_.store(true, std::memory_order_relaxed);
    error_text_ = std::string("stream error: ") + oboe::convertToText(error);
    stream_ = nullptr;  // Oboe has already closed it
    session_id_ = 0;
}

}  // namespace openay
