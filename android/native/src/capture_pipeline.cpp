// Capture pipeline implementation (see capture_pipeline.h).
#include "openay/capture_pipeline.h"

#include <algorithm>
#include <chrono>

#include "openay/opus_codec.h"
#include "openay/transport.h"

namespace openay {

// ---------------------------------------------------------------------------
// IPacketSink adapters around the existing, fully-tested transports.
// ---------------------------------------------------------------------------

namespace {

class UdpPacketSink : public IPacketSink {
public:
    explicit UdpPacketSink(const std::string& host, uint16_t port)
        : udp_(host, port) {}
    bool Send(const Packet& p) override { return udp_.Send(p); }
    bool Valid() const override { return udp_.Valid(); }

private:
    UdpSender udp_;
};

class TcpPacketSink : public IPacketSink {
public:
    explicit TcpPacketSink(const std::string& host, uint16_t port)
        : tcp_(host, port) {}
    bool Send(const Packet& p) override { return tcp_.Send(p); }
    bool Valid() const override { return tcp_.Valid(); }

private:
    TcpClient tcp_;
};

}  // namespace

// ---------------------------------------------------------------------------
// Configure / Start / Stop
// ---------------------------------------------------------------------------

CapturePipeline::CapturePipeline() = default;
CapturePipeline::~CapturePipeline() { Stop(); }

bool CapturePipeline::Configure(IAudioSource* source, TransportType transport,
                                const std::string& host, uint16_t port,
                                CodecType codec, int frame_ms,
                                size_t ring_capacity_bytes) {
    Stop();  // any prior run

    last_error_.store(static_cast<int>(PipelineError::None),
                      std::memory_order_relaxed);
    packets_sent_.store(0, std::memory_order_relaxed);
    bytes_sent_.store(0, std::memory_order_relaxed);
    encode_errors_.store(0, std::memory_order_relaxed);
    send_errors_.store(0, std::memory_order_relaxed);
    callback_idx_.store(0, std::memory_order_relaxed);
    level_peak_.store(0, std::memory_order_relaxed);  // no stale level from a prior run

    // --- validate ----------------------------------------------------------
    if (!source) {
        last_error_.store(static_cast<int>(PipelineError::InvalidConfig),
                          std::memory_order_relaxed);
        return false;
    }
    if (host.empty() || port == 0) {
        last_error_.store(static_cast<int>(PipelineError::InvalidConfig),
                          std::memory_order_relaxed);
        return false;
    }
    if (frame_ms != 5 && frame_ms != 10) {
        last_error_.store(static_cast<int>(PipelineError::InvalidConfig),
                          std::memory_order_relaxed);
        return false;
    }
#ifdef OPENAY_HAVE_OPUS
    if (codec == CodecType::Opus && frame_ms != 10) {
        // shared/protocol.md: Opus is exactly one packet per 10 ms frame.
        last_error_.store(static_cast<int>(PipelineError::InvalidConfig),
                          std::memory_order_relaxed);
        return false;
    }
#else
    if (codec == CodecType::Opus) {
        last_error_.store(static_cast<int>(PipelineError::UnsupportedCodec),
                          std::memory_order_relaxed);
        return false;
    }
#endif

    frame_ms_ = frame_ms;
    frame_samples_ = static_cast<size_t>(frame_ms) * 48;
    frame_bytes_ = frame_samples_ * 2;
    if (ring_capacity_bytes < frame_bytes_ * 2) {
        ring_capacity_bytes = frame_bytes_ * 2;
    }
    // The ring rounds up to a power of two itself.
    ring_ = std::make_unique<SpscRingBuffer>(ring_capacity_bytes);
    scratch_.resize(frame_bytes_);
    pcm_bytes_.resize(frame_bytes_);
    pcm_samples_.resize(frame_samples_);

    // --- sink --------------------------------------------------------------
    if (transport == TransportType::Udp) {
        sink_ = std::make_unique<UdpPacketSink>(host, port);
    } else {
        sink_ = std::make_unique<TcpPacketSink>(host, port);
    }
    if (!sink_->Valid()) {
        last_error_.store(static_cast<int>(PipelineError::SinkOpen),
                          std::memory_order_relaxed);
        return false;
    }

    // --- codec -------------------------------------------------------------
#ifdef OPENAY_HAVE_OPUS
    if (codec == CodecType::Opus) {
        opus_enc_ = std::make_unique<OpusEncoder>();
        if (!opus_enc_->Valid()) {
            last_error_.store(static_cast<int>(PipelineError::Encode),
                              std::memory_order_relaxed);
            return false;
        }
    } else {
        opus_enc_.reset();
    }
#endif

    source_ = source;
    transport_ = transport;
    codec_ = codec;
    host_ = host;
    port_ = port;
    source_->SetCallback(
        [this](const int16_t* s, size_t f) { OnAudio(s, f); });
    return true;
}

bool CapturePipeline::Start() {
    if (running_.load(std::memory_order_relaxed)) return true;
    if (!source_ || !ring_ || !sink_) {
        last_error_.store(static_cast<int>(PipelineError::InvalidConfig),
                          std::memory_order_relaxed);
        return false;
    }
    stop_.store(false, std::memory_order_relaxed);
    if (!source_->Start()) {
        last_error_.store(static_cast<int>(PipelineError::SourceStart),
                          std::memory_order_relaxed);
        return false;
    }
    running_.store(true, std::memory_order_relaxed);
    net_thread_ = std::thread(&CapturePipeline::NetworkLoop, this);
    return true;
}

void CapturePipeline::Stop() {
    if (!running_.load(std::memory_order_relaxed)) {
        // Still release a previous run's resources (sink fd, callback).
        if (source_) source_->Stop();
        sink_.reset();
        return;
    }
    // 1) Stop the source: no further callbacks can run after this returns.
    source_->Stop();
    // 2) Ask the network thread to drain whatever remains, then join.
    stop_.store(true, std::memory_order_relaxed);
    if (net_thread_.joinable()) net_thread_.join();
    running_.store(false, std::memory_order_relaxed);
    // 3) Close the sink.
    sink_.reset();
}

// ---------------------------------------------------------------------------
// RT callback: int16 -> little-endian bytes -> lock-free ring Push.
// ---------------------------------------------------------------------------

void CapturePipeline::OnAudio(const int16_t* samples, size_t frames) {
    // Hard-RT path: level peak + conversion + ring write only (atomics, no
    // allocation). The peak update is a relaxed CAS-max — see level_peak_ in
    // the header for the ordering rationale.
    const auto t0 = std::chrono::steady_clock::now();
    const size_t bytes = frames * 2;
    uint32_t peak = level_peak_.load(std::memory_order_relaxed);
    if (bytes <= scratch_.size()) {  // always true: scratch_ is the max frame
        for (size_t i = 0; i < frames; ++i) {
            const int16_t s = samples[i];
            // |s| as uint32 (int32 intermediate so INT16_MIN negates cleanly),
            // clamped to the documented 0..32767 meter range.
            const uint32_t mag =
                s < 0 ? static_cast<uint32_t>(-static_cast<int32_t>(s))
                      : static_cast<uint32_t>(s);
            const uint32_t mag_clamped = mag > 32767u ? 32767u : mag;
            // CAS-max accumulator (C++17 has no fetch_max). On failure the
            // compare_exchange reloads `peak` with the stored value and we
            // retry; bounded — each retry either succeeds or observes a value
            // >= mag_clamped and exits. The reader's exchange(0) only ever
            // lowers the stored value below mag_clamped, so convergence is
            // guaranteed.
            while (mag_clamped > peak &&
                   !level_peak_.compare_exchange_weak(
                       peak, mag_clamped, std::memory_order_relaxed)) {
            }
            const uint16_t u = static_cast<uint16_t>(s);
            scratch_[2 * i] = static_cast<uint8_t>(u & 0xFFu);
            scratch_[2 * i + 1] = static_cast<uint8_t>(u >> 8);
        }
        // Drop-whole-block semantics: 0 on full, counted inside the ring.
        ring_->Push(scratch_.data(), bytes);
    }
    const auto t1 = std::chrono::steady_clock::now();
    const int64_t us =
        std::chrono::duration_cast<std::chrono::microseconds>(t1 - t0).count();
    const size_t idx =
        callback_idx_.fetch_add(1, std::memory_order_relaxed) % kCallbackSlots;
    callback_us_[idx].store(static_cast<uint32_t>(us),
                            std::memory_order_relaxed);
}

// ---------------------------------------------------------------------------
// Network thread: pop -> encode -> Packet{seq++} -> sink->Send().
// ---------------------------------------------------------------------------

void CapturePipeline::NetworkLoop() {
    Packet pkt;
    pkt.type = (codec_ == CodecType::Opus) ? PayloadType::Opus : PayloadType::Pcm;
    uint16_t seq = 0;
    const size_t needed = frame_bytes_;
    // Poll granularity 1 ms (requirement: wait step <= 5 ms).
    const auto step = std::chrono::milliseconds(1);

    while (!stop_.load(std::memory_order_relaxed)) {
        if (ring_->Available() >= needed) {
            ProcessFrame(&seq, &pkt);  // failure counted inside, stream lives
        } else {
            std::this_thread::sleep_for(step);
        }
    }

    // Drain: the source has been stopped; send whatever complete frames
    // remain. Bounded by ring contents; hard 50 ms deadline.
    const auto deadline =
        std::chrono::steady_clock::now() + std::chrono::milliseconds(50);
    while (ring_->Available() >= needed &&
           std::chrono::steady_clock::now() < deadline) {
        ProcessFrame(&seq, &pkt);
    }
}

bool CapturePipeline::ProcessFrame(uint16_t* seq, Packet* pkt) {
    if (ring_->Pop(pcm_bytes_.data(), frame_bytes_) != frame_bytes_) {
        return false;  // unreachable given the Available() gate
    }
    if (codec_ == CodecType::Opus) {
        // little-endian bytes -> native int16 samples for the encoder.
        for (size_t i = 0; i < frame_samples_; ++i) {
            pcm_samples_[i] = static_cast<int16_t>(
                static_cast<uint16_t>(pcm_bytes_[2 * i]) |
                (static_cast<uint16_t>(pcm_bytes_[2 * i + 1]) << 8));
        }
        if (!opus_enc_->Encode(pcm_samples_.data(), &pkt->payload)) {
            encode_errors_.fetch_add(1, std::memory_order_relaxed);
            last_error_.store(static_cast<int>(PipelineError::Encode),
                              std::memory_order_relaxed);
            return false;
        }
    } else {
        pkt->payload.assign(pcm_bytes_.begin(), pcm_bytes_.end());
    }
    pkt->seq = *seq;
    ++*seq;  // uint16 wraps modulo 65536 per protocol
    if (!sink_->Send(*pkt)) {
        send_errors_.fetch_add(1, std::memory_order_relaxed);
        last_error_.store(static_cast<int>(PipelineError::Send),
                          std::memory_order_relaxed);
        return false;
    }
    packets_sent_.fetch_add(1, std::memory_order_relaxed);
    bytes_sent_.fetch_add(pkt->payload.size(), std::memory_order_relaxed);
    return true;
}

uint64_t CapturePipeline::callback_us_p50() const {
    const size_t n = std::min<size_t>(
        callback_idx_.load(std::memory_order_relaxed), kCallbackSlots);
    if (n < 2) return 0;  // not trivially measurable yet
    uint32_t vals[kCallbackSlots];
    for (size_t i = 0; i < n; ++i) {
        vals[i] = callback_us_[i].load(std::memory_order_relaxed);
    }
    std::sort(vals, vals + n);
    return vals[n / 2];
}

}  // namespace openay
