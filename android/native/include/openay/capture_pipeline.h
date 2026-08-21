// OpenAY Mic — capture pipeline: RT audio callback -> lock-free SPSC ring ->
// dedicated network thread (encode + send).
//
// Threading model
// ---------------
//   audio/RT thread  : IAudioSource::Deliver -> OnAudio(): CAS-max input
//                      level peak + byte conversion -> ring Push(). ONLY
//                      lock-free atomic ops here (hard RT constraint: no I/O,
//                      no malloc, no mutex, no logging).
//   network thread   : polls the ring (1 ms granularity, <= 5 ms wait), pops
//                      frame_ms*48 samples, encodes (PCM passthrough / Opus),
//                      builds Packet{type, seq++ from 0, payload} and sends it
//                      through the configured IPacketSink.
//   control thread   : Configure()/Start()/Stop()/stats accessors.
//
// Error policy: nothing is silently swallowed. Send/encode failures set a
// sticky last_error() code and bump the matching counter; Healthy() turns
// false the moment any error is recorded (a transient codec/network hiccup is
// counted but does not kill the stream). Configure() resets everything.
//
// Lifecycle contract: Configure() must be called while stopped. Start()
// launches the source then the network thread. Stop() stops the source first
// (guaranteeing no further callbacks), lets the network thread drain the ring
// (bounded by its contents — the source no longer feeds it — with a hard
// 50 ms drain deadline), joins the thread, then closes the sink.
//
// Config rules: frame_ms is 5 or 10. Opus is 10 ms only (shared/protocol.md:
// "Opus: exactly one Opus packet per audio frame; 10 ms frames"); requesting
// Opus with frame_ms 5 fails Configure with PipelineError::InvalidConfig.
#ifndef OPENAY_CAPTURE_PIPELINE_H
#define OPENAY_CAPTURE_PIPELINE_H

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <thread>
#include <vector>

#include "openay/audio_source.h"
#include "openay/protocol.h"
#include "openay/ring_buffer.h"

namespace openay {

class OpusEncoder;  // opaque handle; only capture_pipeline.cpp touches it

enum class TransportType { Udp, Tcp };
enum class CodecType { Pcm, Opus };

// Sticky pipeline error codes, exposed via last_error() / PipelineErrorName.
enum class PipelineError : int {
    None = 0,
    InvalidConfig = 1,    // bad frame_ms/codec combination, empty host, ...
    SinkOpen = 2,         // sink could not reach host:port
    SourceStart = 3,      // IAudioSource::Start() failed
    UnsupportedCodec = 4, // Opus requested but the codec is not built in
    Encode = 5,           // codec failure on the network thread
    Send = 6,             // transport failure on the network thread
};

// Exact string names used in StatsJson's "last_error" (fixed vocabulary).
inline const char* PipelineErrorName(PipelineError e) {
    switch (e) {
        case PipelineError::None: return "";
        case PipelineError::InvalidConfig: return "invalid_config";
        case PipelineError::SinkOpen: return "sink_open";
        case PipelineError::SourceStart: return "source_start";
        case PipelineError::UnsupportedCodec: return "unsupported_codec";
        case PipelineError::Encode: return "encode";
        case PipelineError::Send: return "send";
    }
    return "unknown";
}

// Thin transport seam implemented by adapters around the existing
// UdpSender/TcpClient (see capture_pipeline.cpp). Send() must not block
// indefinitely; UDP never does, TCP sends on the network thread only.
struct IPacketSink {
    virtual ~IPacketSink() = default;
    virtual bool Send(const Packet& packet) = 0;
    virtual bool Valid() const = 0;
};

class CapturePipeline {
public:
    // Both ctor and dtor are out-of-line (defined in capture_pipeline.cpp):
    // the implicitly-inline default ctor would instantiate the destructors of
    // all members — including unique_ptr<OpusEncoder> — in every TU, which
    // requires the complete OpusEncoder type. The .cpp has it.
    CapturePipeline();
    ~CapturePipeline();
    CapturePipeline(const CapturePipeline&) = delete;
    CapturePipeline& operator=(const CapturePipeline&) = delete;

    // Creates the sink (host:port), the ring, and the codec state; wires
    // `source`'s callback to OnAudio(). Returns false and sets last_error()
    // on any failure. Must be called while stopped.
    bool Configure(IAudioSource* source, TransportType transport,
                   const std::string& host, uint16_t port, CodecType codec,
                   int frame_ms, size_t ring_capacity_bytes = 16384);

    bool Start();  // false + last_error() on source failure
    void Stop();   // idempotent; drains <= 50 ms, joins, closes the sink

    bool IsRunning() const {
        return running_.load(std::memory_order_relaxed);
    }
    // False once any error has been recorded (sticky until next Configure).
    bool Healthy() const {
        return last_error_.load(std::memory_order_relaxed) ==
               static_cast<int>(PipelineError::None);
    }

    PipelineError last_error() const {
        return static_cast<PipelineError>(last_error_.load(std::memory_order_relaxed));
    }

    // Stats (all safe from any thread; counts are monotonic).
    uint64_t packets_sent() const {
        return packets_sent_.load(std::memory_order_relaxed);
    }
    uint64_t bytes_sent() const {  // payload bytes only (not headers)
        return bytes_sent_.load(std::memory_order_relaxed);
    }
    uint64_t ring_overruns() const { return ring_ ? ring_->overruns() : 0; }
    uint64_t encode_errors() const {
        return encode_errors_.load(std::memory_order_relaxed);
    }
    uint64_t send_errors() const {
        return send_errors_.load(std::memory_order_relaxed);
    }
    // Median RT-callback duration in microseconds; 0 until >= 2 samples.
    uint64_t callback_us_p50() const;

    // Input level peak since the last call (max absolute sample, 0..32767);
    // atomically consumed AND reset, so each call covers exactly its own
    // interval (one UI poll period). RT-thread safe; no effect on the stream.
    uint16_t ExchangeLevelPeak() {
        return static_cast<uint16_t>(
            level_peak_.exchange(0, std::memory_order_relaxed));
    }

private:
    void OnAudio(const int16_t* samples, size_t frames);  // RT thread
    void NetworkLoop();                                   // network thread
    // Pops one frame, encodes, sends; true on full success. On failure the
    // error counters + last_error are set (frame is dropped, stream lives).
    bool ProcessFrame(uint16_t* seq, Packet* pkt);

    IAudioSource* source_ = nullptr;
    std::unique_ptr<SpscRingBuffer> ring_;
    std::unique_ptr<IPacketSink> sink_;

    TransportType transport_ = TransportType::Udp;
    CodecType codec_ = CodecType::Pcm;
    int frame_ms_ = 10;
    std::string host_;
    uint16_t port_ = 0;
    size_t frame_samples_ = 0;  // frame_ms * 48
    size_t frame_bytes_ = 0;    // frame_samples_ * 2

    // Fixed-size RT-callback scratch (allocated once at Configure; max frame
    // is 10 ms @ 48 kHz = 960 bytes). No malloc in the callback.
    std::vector<uint8_t> scratch_;
    // Network-thread buffers (same sizes; allocated at Configure).
    std::vector<uint8_t> pcm_bytes_;
    std::vector<int16_t> pcm_samples_;

    std::unique_ptr<OpusEncoder> opus_enc_;  // non-null iff codec == Opus

    std::thread net_thread_;
    std::atomic<bool> running_{false};
    std::atomic<bool> stop_{false};

    // Stats (atomics; written by RT/network threads, read by any thread).
    std::atomic<uint64_t> packets_sent_{0};
    std::atomic<uint64_t> bytes_sent_{0};
    std::atomic<uint64_t> encode_errors_{0};
    std::atomic<uint64_t> send_errors_{0};
    std::atomic<int> last_error_{0};

    // Input level meter: max |sample| seen since the last ExchangeLevelPeak(),
    // 0..32767 (INT16_MIN clamps to 32767). Written on the RT thread with a
    // relaxed compare-and-swap max loop (C++17 has no fetch_max; bounded
    // retries — the accumulator only ever grows or resets to 0, so each retry
    // either succeeds or sees a value >= the candidate and stops); consumed
    // and reset by any thread via exchange(). Relaxed ordering is a deliberate
    // choice: this is self-contained telemetry (a marginally stale/racing peak
    // only shifts one UI poll by a frame), not a synchronization signal, so no
    // release/acquire pairing is needed. RT-safe: lock-free, allocation-free.
    std::atomic<uint32_t> level_peak_{0};

    // Callback-duration histogram (fixed size, RT-safe relaxed atomic
    // writes; median computed on the stats path).
    static constexpr size_t kCallbackSlots = 128;
    std::array<std::atomic<uint32_t>, kCallbackSlots> callback_us_{};
    std::atomic<size_t> callback_idx_{0};
};

}  // namespace openay

#endif  // OPENAY_CAPTURE_PIPELINE_H
