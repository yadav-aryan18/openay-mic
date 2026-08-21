// OpenAY Mic — end-to-end capture pipeline tests.
//
// TestSource(440 Hz) -> CapturePipeline -> in-process UdpReceiver:
//   * PCM, 10 ms frames, 2.0 s: ~200 packets of 960 bytes, seq contiguous
//     from 0, PCM RMS > 1000 (real sine audio, not silence), no overruns,
//     no send errors.
//   * Opus, 10 ms frames, 1.5 s: every payload decodes via the opus codec;
//     decoded RMS (after 2 warmup frames) > 1000.
//   * CaptureEngine facade smoke: Configure/Start/Stop + StatsJson shape.
//
// Ports are scanned inside 43000-43100 (never below, so the parallel bench
// tool's 42000-42999 range and the validation gate's fixed ports are safe).
#include "openay/capture_pipeline.h"
#include "openay/capture_engine.h"
#include "openay/test_source.h"
#include "openay/transport.h"

#ifdef OPENAY_HAVE_OPUS
#include "openay/opus_codec.h"
#endif

#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <memory>
#include <string>
#include <thread>
#include <vector>

using openay::CaptureEngine;
using openay::CapturePipeline;
using openay::CodecType;
using openay::Packet;
using openay::PayloadType;
using openay::TestSource;
using openay::TransportType;
using openay::UdpReceiver;

namespace {

std::atomic<int> g_failures{0};

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            g_failures.fetch_add(1, std::memory_order_relaxed);          \
        }                                                                \
    } while (0)

// Bind the first free port in [43000, 43100]; the receiver stays bound (no
// bind-then-close race). Returns nullptr if the whole range is taken.
std::unique_ptr<UdpReceiver> BindReceiver(uint16_t* port_out) {
    for (uint16_t p = 43000; p <= 43100; ++p) {
        auto recv = std::make_unique<UdpReceiver>(p);
        if (recv->Bind()) {
            *port_out = p;
            return recv;
        }
    }
    return nullptr;
}

// Receives until `stop` is set, then drains until a 1 s idle timeout (catches
// the packets the pipeline flushes during Stop()).
void RunReceiver(std::unique_ptr<UdpReceiver> recv, std::atomic<bool>* stop,
                 std::vector<Packet>* out) {
    while (!stop->load(std::memory_order_relaxed)) {
        Packet p;
        if (recv->Recv(&p, 200)) out->push_back(std::move(p));
    }
    for (;;) {
        Packet p;
        if (!recv->Recv(&p, 1000)) break;  // 1 s idle: stream finished
        out->push_back(std::move(p));
    }
}

// Sample-level helpers on little-endian 16-bit PCM payloads.
inline int16_t SampleAt(const uint8_t* b, size_t i) {
    return static_cast<int16_t>(static_cast<uint16_t>(b[2 * i]) |
                                (static_cast<uint16_t>(b[2 * i + 1]) << 8));
}

double TotalRms(const std::vector<Packet>& pkts) {
    double sq = 0.0;
    size_t count = 0;
    for (const Packet& p : pkts) {
        const size_t n = p.payload.size() / 2;
        for (size_t i = 0; i < n; ++i) {
            const double v = SampleAt(p.payload.data(), i);
            sq += v * v;
            ++count;
        }
    }
    return count > 0 ? std::sqrt(sq / static_cast<double>(count)) : 0.0;
}

// Shared assertions for both variants: seq contiguous from 0, expected type,
// expected payload size (skipped when 0, e.g. variable-size Opus packets), no
// overruns/errors on the pipeline.
void CheckStream(const std::vector<Packet>& pkts, CapturePipeline* pipe,
                 size_t expected_min, size_t expected_max,
                 PayloadType type, size_t payload_size) {
    printf("capture: received=%zu\n", pkts.size());
    CHECK(pkts.size() >= expected_min && pkts.size() <= expected_max);
    for (size_t i = 0; i < pkts.size(); ++i) {
        if (pkts[i].seq != static_cast<uint16_t>(i)) {
            fprintf(stderr, "FAIL: seq %u at index %zu (not contiguous from 0)\n",
                    pkts[i].seq, i);
            g_failures.fetch_add(1, std::memory_order_relaxed);
            break;
        }
        CHECK(pkts[i].type == type);
        if (payload_size != 0) CHECK(pkts[i].payload.size() == payload_size);
    }
    CHECK(pipe->ring_overruns() == 0);
    CHECK(pipe->encode_errors() == 0);
    CHECK(pipe->send_errors() == 0);
    CHECK(pipe->Healthy());
    CHECK(pipe->packets_sent() == pkts.size());
}

// ---------------------------------------------------------------------------
// PCM: 2.0 s at 10 ms -> ~200 packets of 960 bytes; RMS proves real audio.
// ---------------------------------------------------------------------------

void TestPcm() {
    uint16_t port = 0;
    auto recv = BindReceiver(&port);
    CHECK(recv != nullptr);
    if (!recv) return;
    printf("capture: pcm test on port %u\n", port);

    TestSource source;  // 440 Hz, 0.4 * 32767, 480-sample frames
    CapturePipeline pipe;
    CHECK(pipe.Configure(&source, TransportType::Udp, "127.0.0.1", port,
                         CodecType::Pcm, 10));
    CHECK(pipe.Start());

    std::atomic<bool> stop{false};
    std::vector<Packet> pkts;
    std::thread recv_thread(RunReceiver, std::move(recv), &stop, &pkts);

    std::this_thread::sleep_for(std::chrono::milliseconds(2000));
    pipe.Stop();   // source stop -> drain -> join -> close sink
    stop.store(true, std::memory_order_relaxed);
    recv_thread.join();

    CheckStream(pkts, &pipe, 180, 220, PayloadType::Pcm, 960);
    const double rms = TotalRms(pkts);
    printf("capture: pcm rms=%.1f\n", rms);
    CHECK(rms > 1000.0);
}

// ---------------------------------------------------------------------------
// Opus: 1.5 s, every packet decodes; decoded RMS (after warmup) > 1000.
// ---------------------------------------------------------------------------

#ifdef OPENAY_HAVE_OPUS
void TestOpus() {
    uint16_t port = 0;
    auto recv = BindReceiver(&port);
    CHECK(recv != nullptr);
    if (!recv) return;
    printf("capture: opus test on port %u\n", port);

    TestSource source;
    CapturePipeline pipe;
    CHECK(pipe.Configure(&source, TransportType::Udp, "127.0.0.1", port,
                         CodecType::Opus, 10));
    CHECK(pipe.Start());

    std::atomic<bool> stop{false};
    std::vector<Packet> pkts;
    std::thread recv_thread(RunReceiver, std::move(recv), &stop, &pkts);

    std::this_thread::sleep_for(std::chrono::milliseconds(1500));
    pipe.Stop();
    stop.store(true, std::memory_order_relaxed);
    recv_thread.join();

    CheckStream(pkts, &pipe, 120, 165, PayloadType::Opus, 0);  // len varies

    openay::OpusDecoder dec;
    CHECK(dec.Valid());
    double sq = 0.0;
    size_t count = 0;
    size_t warmup_skipped = 0;
    bool all_decoded = true;
    for (const Packet& p : pkts) {
        if (warmup_skipped < 2) {  // skip the encoder/decoder pre-roll frames
            ++warmup_skipped;
            continue;
        }
        int16_t pcm[openay::kOpusFrameSamples];
        if (!dec.Decode(p.payload.data(), p.payload.size(), pcm)) {
            all_decoded = false;
            continue;
        }
        for (int i = 0; i < openay::kOpusFrameSamples; ++i) {
            const double v = static_cast<double>(pcm[i]);
            sq += v * v;
            ++count;
        }
    }
    CHECK(all_decoded);
    const double rms = count > 0 ? std::sqrt(sq / static_cast<double>(count)) : 0.0;
    printf("capture: opus decoded_rms=%.1f (frames=%zu)\n", rms, count / 480);
    CHECK(rms > 1000.0);
}
#endif  // OPENAY_HAVE_OPUS

// ---------------------------------------------------------------------------
// CaptureEngine facade smoke: configure/start/stop + StatsJson exact shape.
// ---------------------------------------------------------------------------

void TestEngineSmoke() {
    // A real bound receiver keeps the connected UDP socket free of ICMP
    // port-unreachable errors; the kernel buffer absorbs the packets (the
    // smoke never reads them).
    uint16_t port = 0;
    auto recv = BindReceiver(&port);
    CHECK(recv != nullptr);
    if (!recv) return;

    CaptureEngine engine;
    CHECK(engine.Configure(TransportType::Udp, "127.0.0.1", port,
                           CodecType::Pcm, 10));
    CHECK(engine.Start());
    CHECK(engine.IsRunning());
    std::this_thread::sleep_for(std::chrono::milliseconds(500));
    CHECK(engine.IsRunning());
    const std::string during = engine.StatsJson();
    printf("capture: engine stats during run:\n%s\n", during.c_str());
    CHECK(during.find("\"running\":true") != std::string::npos);
    CHECK(during.find("\"transport\":\"udp\"") != std::string::npos);
    CHECK(during.find("\"host\":\"127.0.0.1\"") != std::string::npos);
    CHECK(during.find("\"port\":" + std::to_string(port)) != std::string::npos);
    CHECK(during.find("\"codec\":\"pcm\"") != std::string::npos);
    CHECK(during.find("\"frame_ms\":10") != std::string::npos);
    CHECK(during.find("\"sharing\":\"exclusive\"") != std::string::npos);
    CHECK(during.find("\"sample_rate\":48000") != std::string::npos);
    CHECK(during.find("\"last_error\":\"\"") != std::string::npos);

    engine.Stop();
    CHECK(!engine.IsRunning());
    const std::string after = engine.StatsJson();
    CHECK(after.find("\"running\":false") != std::string::npos);
    // Field order contract: "sent" appears before "bytes", "callback_us_p50"
    // before "last_error" (see capture_engine.h).
    const size_t sent_pos = after.find("\"sent\":");
    const size_t bytes_pos = after.find("\"bytes\":");
    const size_t p50_pos = after.find("\"callback_us_p50\":");
    const size_t err_pos = after.find("\"last_error\":");
    CHECK(sent_pos != std::string::npos && bytes_pos != std::string::npos);
    CHECK(sent_pos < bytes_pos);
    CHECK(p50_pos != std::string::npos && err_pos != std::string::npos);
    CHECK(p50_pos < err_pos);

    // A failed configure must report a sticky last_error.
    CHECK(!engine.Configure(TransportType::Udp, "127.0.0.1", port,
                            CodecType::Pcm, 7));  // frame_ms must be 5 or 10
    const std::string failed = engine.StatsJson();
    CHECK(failed.find("\"last_error\":\"invalid_config\"") != std::string::npos);
}

}  // namespace

int main() {
    TestPcm();
#ifdef OPENAY_HAVE_OPUS
    TestOpus();
#else
    printf("capture: opus variant skipped (OPENAY_HAVE_OPUS=0)\n");
#endif
    TestEngineSmoke();
    if (g_failures.load() == 0) {
        printf("test_capture: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "test_capture: %d check(s) FAILED\n", g_failures.load());
    return 1;
}