// Capture engine facade implementation (see capture_engine.h).
#include "openay/capture_engine.h"

#ifdef __ANDROID__
#include "openay/oboe_source.h"
#else
#include "openay/test_source.h"
#endif

#include <cstdint>

namespace openay {
namespace {

const char* TransportName(TransportType t) {
    switch (t) {
        case TransportType::Udp: return "udp";
        case TransportType::Tcp: return "tcp";
        case TransportType::Bt: return "bt";
    }
    return "unknown";
}
const char* CodecName(CodecType c) {
    return c == CodecType::Pcm ? "pcm" : "opus";
}

std::unique_ptr<IAudioSource> MakeSource(bool session_for_effects) {
#ifdef __ANDROID__
    auto src = std::make_unique<OboeSource>();
    src->set_allocate_session(session_for_effects);
    return src;
#else
    (void)session_for_effects;
    return std::make_unique<TestSource>();  // 440 Hz host simulator
#endif
}

// round(peak / 32767 * 100) as an exact integer (0..100); no FP needed on the
// stats path: floor(peak*100/32767 + 0.5) == (peak*200 + 32767) / 65534.
uint32_t LevelPercent(uint32_t peak) {
    return (peak * 200u + 32767u) / 65534u;
}

}  // namespace

CaptureEngine::~CaptureEngine() { Stop(); }

bool CaptureEngine::Configure(TransportType transport, const std::string& host,
                              uint16_t port, CodecType codec, int frame_ms,
                              bool session_for_effects) {
    std::lock_guard<std::mutex> lk(mu_);
    if (running_) StopLocked();

    // Record the attempted config first so a failed Configure still shows the
    // intended transport/host/port/codec next to last_error in StatsJson.
    transport_ = transport;
    host_ = host;
    port_ = port;
    codec_ = codec;
    frame_ms_ = frame_ms;
    sharing_ = "exclusive";

    auto src = MakeSource(session_for_effects);
    auto pipe = std::make_unique<CapturePipeline>();
    if (!pipe->Configure(src.get(), transport, host, port, codec, frame_ms)) {
        // Keep the failed pipeline around so StatsJson reports the reason.
        pipeline_ = std::move(pipe);
        source_ = std::move(src);
        return false;
    }
    // The app's voice-FX toggle: the cleanup/level stage runs in the native
    // pipeline (the platform effects attached by the service are limited to
    // echo cancellation).
    pipe->SetVoiceFx(session_for_effects);
    pipeline_ = std::move(pipe);
    source_ = std::move(src);
    return true;
}

bool CaptureEngine::ConfigureBt(int fd, const std::string& device_address,
                                CodecType codec, int frame_ms,
                                bool session_for_effects) {
    std::lock_guard<std::mutex> lk(mu_);
    if (running_) StopLocked();

    // Record the attempted config first so a failed Configure still shows the
    // intended transport/device/codec next to last_error in StatsJson. The
    // fd is owned by the pipeline's sink from entry (closed on Stop or when
    // a later Configure replaces this pipeline); device_address is
    // display-only and port has no meaning over RFCOMM.
    transport_ = TransportType::Bt;
    host_ = device_address;
    port_ = 0;
    codec_ = codec;
    frame_ms_ = frame_ms;
    sharing_ = "exclusive";

    auto src = MakeSource(session_for_effects);
    auto pipe = std::make_unique<CapturePipeline>();
    if (!pipe->ConfigureBt(src.get(), fd, codec, frame_ms)) {
        // Keep the failed pipeline around so StatsJson reports the reason.
        // The fd was consumed either way (closed by the configure attempt).
        pipeline_ = std::move(pipe);
        source_ = std::move(src);
        return false;
    }
    pipe->SetVoiceFx(session_for_effects);
    pipeline_ = std::move(pipe);
    source_ = std::move(src);
    return true;
}

bool CaptureEngine::Start() {
    std::lock_guard<std::mutex> lk(mu_);
    if (running_) return true;
    if (!pipeline_) return false;
    if (!pipeline_->Start()) return false;
    running_ = true;
    if (source_) sharing_ = source_->sharing_mode();
    return true;
}

void CaptureEngine::Stop() {
    std::lock_guard<std::mutex> lk(mu_);
    StopLocked();
}

void CaptureEngine::StopLocked() {
    if (pipeline_) pipeline_->Stop();  // stops source, drains, joins, closes sink
    running_ = false;
    sharing_ = "exclusive";
}

bool CaptureEngine::IsRunning() const {
    std::lock_guard<std::mutex> lk(mu_);
    return running_;
}

void CaptureEngine::SetMuted(bool muted) {
    std::lock_guard<std::mutex> lk(mu_);
    if (pipeline_) pipeline_->SetMuted(muted);
}

int CaptureEngine::AudioSessionId() {
    std::lock_guard<std::mutex> lk(mu_);
    if (!source_) return 0;
    return source_->audio_session_id();
}

uint16_t CaptureEngine::ExchangeLevelPeak() {
    std::lock_guard<std::mutex> lk(mu_);
    if (!running_ || !pipeline_) return 0;
    return pipeline_->ExchangeLevelPeak();
}

std::string CaptureEngine::StatsJson() const {
    std::lock_guard<std::mutex> lk(mu_);

    const bool running = running_;
    uint64_t sent = 0, bytes = 0, overruns = 0, encode_err = 0, send_err = 0;
    uint64_t xruns = 0, p50 = 0;
    uint32_t level_peak = 0;
    std::string last_error;
    if (pipeline_) {
        sent = pipeline_->packets_sent();
        bytes = pipeline_->bytes_sent();
        overruns = pipeline_->ring_overruns();
        encode_err = pipeline_->encode_errors();
        send_err = pipeline_->send_errors();
        p50 = pipeline_->callback_us_p50();
        const PipelineError pe = pipeline_->last_error();
        if (pe != PipelineError::None) last_error = PipelineErrorName(pe);
    }
    // Reading for stats consumes the peak (exchange semantics): each poll
    // reports exactly its own interval. 0 when not running.
    if (running && pipeline_) level_peak = pipeline_->ExchangeLevelPeak();
    if (source_) {
        if (last_error.empty()) last_error = source_->error_string();
        xruns = source_->xruns();
    }

    // Hand-serialized, exact field order (see capture_engine.h). The fields
    // are fixed vocabulary; nothing needs escaping.
    std::string json = "{\"running\":";
    json += running ? "true" : "false";
    json += ",\"transport\":\"";
    json += TransportName(transport_);
    json += "\",\"host\":\"";
    json += host_;
    json += "\",\"port\":";
    json += std::to_string(port_);
    json += ",\"codec\":\"";
    json += CodecName(codec_);
    json += "\",\"frame_ms\":";
    json += std::to_string(frame_ms_);
    json += ",\"sharing\":\"";
    json += sharing_;
    json += "\",\"sample_rate\":48000";
    json += ",\"sent\":";
    json += std::to_string(sent);
    json += ",\"bytes\":";
    json += std::to_string(bytes);
    json += ",\"ring_overruns\":";
    json += std::to_string(overruns);
    json += ",\"encode_errors\":";
    json += std::to_string(encode_err);
    json += ",\"send_errors\":";
    json += std::to_string(send_err);
    json += ",\"xruns\":";
    json += std::to_string(xruns);
    json += ",\"callback_us_p50\":";
    json += std::to_string(p50);
    json += ",\"last_error\":\"";
    json += last_error;
    json += "\",\"level_peak\":";
    json += std::to_string(LevelPercent(level_peak));
    json += ",\"peer_link\":\"";
    json += pipeline_ ? PeerLinkName(pipeline_->peer_link()) : "unknown";
    json += "\",\"muted\":";
    json += pipeline_ && pipeline_->IsMuted() ? "true" : "false";
    json += ",\"audio_session\":";
    json += std::to_string(source_ ? source_->audio_session_id() : 0);
    json += "}";
    return json;
}

}  // namespace openay
