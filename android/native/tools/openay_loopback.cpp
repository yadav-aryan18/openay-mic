// OpenAY Mic — loopback/benchmark CLI for the native core.
//
// Deterministic xorshift32 filler per shared/protocol.md:
//   state = seq; per byte: state ^= state<<13; state ^= state>>17;
//   state ^= state<<5; emit state & 0xFF.
// Bench payloads prefix an 8-byte little-endian CLOCK_MONOTONIC ns timestamp.
#include "openay/capture_pipeline.h"
#include "openay/protocol.h"
#include "openay/stats.h"
#include "openay/test_source.h"
#include "openay/transport.h"

#include <atomic>
#include <chrono>
#include <cinttypes>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <ctime>
#include <functional>
#include <memory>
#include <string>
#include <thread>
#include <vector>

using openay::FormatStats;
using openay::kHeaderLen;
using openay::Packet;
using openay::PacketStats;
using openay::PayloadType;
using openay::SeqEvent;
using openay::SeqTracker;
using openay::TcpClient;
using openay::TcpConn;
using openay::TcpServer;
using openay::UdpReceiver;
using openay::UdpSender;

namespace {

void Usage(FILE* f) {
    fprintf(f,
            "usage: openay_loopback <command> [args]\n"
            "  send-udp <host> <port> <count> [payload_size=480] [interval_us=0]\n"
            "  recv-udp <port> <count> [payload_size=480]\n"
            "  send-tcp <host> <port> <count> [payload_size=480]\n"
            "  recv-tcp <port> <count> [payload_size=480]\n"
            "  bench <udp|tcp> <port> <count> [payload_size=480]\n"
            "  tone-udp <host> <port> <seconds> [freq=440] [codec=pcm]\n"
            "    streams REAL sine audio (TestSource -> CapturePipeline, 10 ms\n"
            "    frames) as OpenAY packets; prints TONE stats and exits 0 iff\n"
            "    the stream stayed healthy (no overruns, no send/encode errors).\n");
}

uint64_t NowNs() {
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<uint64_t>(ts.tv_sec) * 1000000000ull +
           static_cast<uint64_t>(ts.tv_nsec);
}

void StoreU64LE(uint8_t* p, uint64_t v) {
    for (int i = 0; i < 8; ++i) p[i] = static_cast<uint8_t>(v >> (8 * i));
}

uint64_t LoadU64LE(const uint8_t* p) {
    uint64_t v = 0;
    for (int i = 7; i >= 0; --i) v = (v << 8) | p[i];
    return v;
}

void FillXorshift(uint32_t seed, uint8_t* out, size_t n) {
    uint32_t state = seed;
    for (size_t i = 0; i < n; ++i) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        out[i] = static_cast<uint8_t>(state & 0xFFu);
    }
}

bool VerifyXorshift(uint32_t seed, const uint8_t* data, size_t n) {
    uint32_t state = seed;
    for (size_t i = 0; i < n; ++i) {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        if (data[i] != static_cast<uint8_t>(state & 0xFFu)) return false;
    }
    return true;
}

bool ParseU16(const char* s, uint16_t* out) {
    char* end = nullptr;
    const long v = strtol(s, &end, 10);
    if (!end || *end != '\0' || v < 0 || v > 65535) return false;
    *out = static_cast<uint16_t>(v);
    return true;
}

// payload_size is bounded by the 16-bit length field.
bool ParseSize(const char* s, size_t* out) {
    char* end = nullptr;
    const unsigned long long v = strtoull(s, &end, 10);
    if (!end || *end != '\0' || v > 65535) return false;
    *out = static_cast<size_t>(v);
    return true;
}

bool ParseCount(const char* s, size_t* out) {
    char* end = nullptr;
    const unsigned long long v = strtoull(s, &end, 10);
    if (!end || *end != '\0') return false;
    *out = static_cast<size_t>(v);
    return true;
}

// Alternating Pcm/Opus payload with xorshift(seq) content (send side).
void BuildAlternatingPacket(size_t index, size_t payload_size,
                            std::vector<uint8_t>* scratch, Packet* pkt) {
    pkt->type = (index % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
    pkt->seq = static_cast<uint16_t>(index & 0xFFFFu);
    scratch->resize(payload_size);
    FillXorshift(pkt->seq, scratch->data(), scratch->size());
    pkt->payload = *scratch;
}

// Bench payload: 8-byte LE monotonic-ns timestamp + xorshift(seq) remainder.
void BuildBenchPacket(size_t index, size_t payload_size,
                      std::vector<uint8_t>* scratch, Packet* pkt) {
    pkt->type = (index % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
    pkt->seq = static_cast<uint16_t>(index & 0xFFFFu);
    scratch->resize(payload_size);
    StoreU64LE(scratch->data(), NowNs());
    FillXorshift(pkt->seq, scratch->data() + 8, scratch->size() - 8);
    pkt->payload = *scratch;
}

int SendUdp(const std::string& host, uint16_t port, size_t count,
            size_t payload_size, size_t interval_us) {
    UdpSender sender(host, port);
    if (!sender.Valid()) {
        fprintf(stderr, "openay: send-udp: could not resolve/connect %s:%u\n",
                host.c_str(), port);
        return 1;
    }
    std::vector<uint8_t> scratch;
    Packet pkt;
    for (size_t i = 0; i < count; ++i) {
        BuildAlternatingPacket(i, payload_size, &scratch, &pkt);
        if (!sender.Send(pkt)) {
            fprintf(stderr, "openay: send-udp: send failed at packet %zu\n", i);
            return 1;
        }
        if (interval_us > 0) {
            std::this_thread::sleep_for(std::chrono::microseconds(interval_us));
        }
    }
    printf("SENT count=%zu bytes=%zu\n", count, count * (kHeaderLen + payload_size));
    return 0;
}

int SendTcp(const std::string& host, uint16_t port, size_t count, size_t payload_size) {
    TcpClient client(host, port);
    if (!client.Valid()) {
        fprintf(stderr, "openay: send-tcp: could not connect to %s:%u\n",
                host.c_str(), port);
        return 1;
    }
    std::vector<uint8_t> scratch;
    Packet pkt;
    for (size_t i = 0; i < count; ++i) {
        BuildAlternatingPacket(i, payload_size, &scratch, &pkt);
        if (!client.Send(pkt)) {
            fprintf(stderr, "openay: send-tcp: send failed at packet %zu\n", i);
            return 1;
        }
    }
    printf("SENT count=%zu bytes=%zu\n", count, count * (kHeaderLen + payload_size));
    return 0;
}

// Shared verification loop for recv-udp / recv-tcp: receive until `count`
// valid packets or 15 s idle; verify type alternation (Pcm,Opus,... starting
// Pcm), seq contiguity from 0, payload length and byte-exact xorshift(seq)
// content. Mismatches increment content_errors. Final line is FormatStats.
// Returns 0 iff received==count and all error counters are 0.
int RecvLoop(const std::function<bool(Packet*, int timeout_ms)>& recv_fn,
             const std::function<const PacketStats&()>& stats_fn,
             const std::function<bool()>& eof_fn, size_t count,
             size_t payload_size) {
    const auto kIdle = std::chrono::milliseconds(15000);
    SeqTracker tool_tracker;
    uint64_t content_errors = 0;
    size_t valid = 0;
    PacketStats prev = stats_fn();
    auto last_activity = std::chrono::steady_clock::now();

    while (valid < count) {
        const auto now = std::chrono::steady_clock::now();
        const auto idle = now - last_activity;
        if (idle >= kIdle) {
            fprintf(stderr, "openay: recv: 15 s idle (%zu/%zu valid packets)\n",
                    valid, count);
            break;
        }
        const int timeout_ms = static_cast<int>(
            std::chrono::duration_cast<std::chrono::milliseconds>(kIdle - idle).count());
        Packet pkt;
        if (recv_fn(&pkt, timeout_ms)) {
            last_activity = std::chrono::steady_clock::now();
            ++valid;
            const PayloadType expect =
                (pkt.seq % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
            if (pkt.type != expect) {
                fprintf(stderr,
                        "openay: recv: seq=%u type=%d expected %d (alternation "
                        "starting Pcm)\n",
                        pkt.seq, static_cast<int>(pkt.type), static_cast<int>(expect));
                ++content_errors;
            }
            if (pkt.payload.size() != payload_size) {
                fprintf(stderr, "openay: recv: seq=%u payload len=%zu expected %zu\n",
                        pkt.seq, pkt.payload.size(), payload_size);
                ++content_errors;
            } else if (!VerifyXorshift(pkt.seq, pkt.payload.data(), payload_size)) {
                fprintf(stderr,
                        "openay: recv: seq=%u payload content mismatch "
                        "(xorshift seed=seq)\n",
                        pkt.seq);
                ++content_errors;
            }
            uint16_t gap = 0;
            if (tool_tracker.Update(pkt.seq, &gap) != SeqEvent::InOrder) {
                fprintf(stderr, "openay: recv: seq=%u breaks contiguity from 0\n",
                        pkt.seq);
                ++content_errors;
            }
        } else {
            const PacketStats& s = stats_fn();
            if (s.malformed != prev.malformed) {
                prev = s;
                last_activity = std::chrono::steady_clock::now();
            } else if (eof_fn && eof_fn()) {
                fprintf(stderr, "openay: recv: stream closed by peer\n");
                break;
            }
        }
    }

    PacketStats total = stats_fn();
    total.content_errors = content_errors;
    printf("%s\n", FormatStats(total).c_str());
    const bool ok = total.received == count && total.lost == 0 && total.duplicate == 0 &&
                    total.out_of_order == 0 && total.malformed == 0 &&
                    total.content_errors == 0;
    return ok ? 0 : 1;
}

int RecvUdp(uint16_t port, size_t count, size_t payload_size) {
    UdpReceiver recv(port);
    if (!recv.Bind()) {
        fprintf(stderr, "openay: recv-udp: bind failed on port %u\n", port);
        return 1;
    }
    return RecvLoop(
        [&recv](Packet* p, int timeout_ms) { return recv.Recv(p, timeout_ms); },
        [&recv]() -> const PacketStats& { return recv.stats(); },
        []() { return false; }, count, payload_size);
}

int RecvTcp(uint16_t port, size_t count, size_t payload_size) {
    TcpServer server(port);
    if (!server.Listen()) {
        fprintf(stderr, "openay: recv-tcp: listen failed on port %u\n", port);
        return 1;
    }
    std::unique_ptr<TcpConn> conn = server.Accept(15000);
    if (!conn) {
        fprintf(stderr, "openay: recv-tcp: no connection within 15 s\n");
        return 1;
    }
    return RecvLoop(
        [&conn](Packet* p, int timeout_ms) { return conn->Recv(p, timeout_ms); },
        [&conn]() -> const PacketStats& { return conn->stats(); },
        [&conn]() { return conn->Eof(); }, count, payload_size);
}

uint64_t Percentile(const std::vector<uint64_t>& sorted, int pct) {
    if (sorted.empty()) return 0;
    size_t idx = static_cast<size_t>((static_cast<double>(pct) / 100.0) *
                                     static_cast<double>(sorted.size()));
    if (idx == 0) idx = 1;
    if (idx > sorted.size()) idx = sorted.size();
    return sorted[idx - 1];
}

int Bench(const std::string& transport, uint16_t port, size_t count,
          size_t payload_size) {
    if (payload_size < 8) {
        fprintf(stderr,
                "openay: bench: payload_size must be >= 8 (8-byte timestamp "
                "prefix)\n");
        return 1;
    }
    std::atomic<bool> ready{false};
    std::atomic<bool> recv_failed{false};
    std::vector<uint64_t> deltas_us;

    std::thread recv_thread([&]() {
        if (transport == "udp") {
            UdpReceiver recv(port);
            if (!recv.Bind()) {
                recv_failed = true;
                ready = true;
                return;
            }
            ready = true;
            for (size_t i = 0; i < count; ++i) {
                Packet pkt;
                if (!recv.Recv(&pkt, 10000)) {
                    fprintf(stderr, "openay: bench: udp receiver timeout at %zu/%zu\n",
                            i, count);
                    recv_failed = true;
                    break;
                }
                if (pkt.payload.size() < 8) continue;
                const uint64_t send_ns = LoadU64LE(pkt.payload.data());
                const uint64_t now_ns = NowNs();
                deltas_us.push_back((now_ns >= send_ns ? now_ns - send_ns : 0) / 1000);
            }
        } else {
            TcpServer server(port);
            if (!server.Listen()) {
                recv_failed = true;
                ready = true;
                return;
            }
            ready = true;
            std::unique_ptr<TcpConn> conn = server.Accept(10000);
            if (!conn) {
                fprintf(stderr, "openay: bench: tcp accept timeout\n");
                recv_failed = true;
                return;
            }
            for (size_t i = 0; i < count; ++i) {
                Packet pkt;
                if (!conn->Recv(&pkt, 10000)) {
                    fprintf(stderr, "openay: bench: tcp receiver timeout at %zu/%zu\n",
                            i, count);
                    recv_failed = true;
                    break;
                }
                if (pkt.payload.size() < 8) continue;
                const uint64_t send_ns = LoadU64LE(pkt.payload.data());
                const uint64_t now_ns = NowNs();
                deltas_us.push_back((now_ns >= send_ns ? now_ns - send_ns : 0) / 1000);
            }
        }
    });

    while (!ready.load()) std::this_thread::yield();

    bool send_ok = true;
    std::vector<uint8_t> scratch;
    Packet pkt;
    const auto send_one = [&](size_t i) {
        BuildBenchPacket(i, payload_size, &scratch, &pkt);
    };
    if (transport == "udp") {
        UdpSender sender("127.0.0.1", port);
        send_ok = sender.Valid();
        for (size_t i = 0; send_ok && i < count; ++i) {
            send_one(i);
            send_ok = sender.Send(pkt);
        }
    } else if (transport == "tcp") {
        TcpClient client("127.0.0.1", port);
        send_ok = client.Valid();
        for (size_t i = 0; send_ok && i < count; ++i) {
            send_one(i);
            send_ok = client.Send(pkt);
        }
    } else {
        fprintf(stderr, "openay: bench: unknown transport '%s' (use udp|tcp)\n",
                transport.c_str());
        send_ok = false;
    }

    recv_thread.join();
    if (!send_ok) {
        fprintf(stderr, "openay: bench: sender failed\n");
        return 1;
    }
    if (recv_failed) {
        fprintf(stderr, "openay: bench: receiver failed\n");
        return 1;
    }
    if (deltas_us.size() != count) {
        fprintf(stderr, "openay: bench: received %zu of %zu packets\n",
                deltas_us.size(), count);
        return 1;
    }
    std::sort(deltas_us.begin(), deltas_us.end());
    const uint64_t p50 = Percentile(deltas_us, 50);
    const uint64_t p95 = Percentile(deltas_us, 95);
    const uint64_t p99 = Percentile(deltas_us, 99);
    const uint64_t max_us = deltas_us.back();
    printf("BENCH transport=%s count=%zu p50_us=%llu p95_us=%llu p99_us=%llu "
           "max_us=%llu\n",
           transport.c_str(), count, static_cast<unsigned long long>(p50),
           static_cast<unsigned long long>(p95),
           static_cast<unsigned long long>(p99),
           static_cast<unsigned long long>(max_us));
    return p99 < 5000 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// tone-udp: stream REAL sine audio (TestSource -> CapturePipeline) as OpenAY
// packets. This is the validation driver for the desktop-side receiver: it
// exercises the exact Phase 3 capture path (RT callback -> ring -> network
// thread -> UdpSender) with real audio content, not deterministic filler.
// ---------------------------------------------------------------------------

bool ParseDouble(const char* s, double* out) {
    char* end = nullptr;
    const double v = strtod(s, &end);
    if (!end || *end != '\0' || v < 0) return false;
    *out = v;
    return true;
}

int ToneUdp(const std::string& host, uint16_t port, double seconds,
            double freq, const std::string& codec) {
    const openay::CodecType ct =
        codec == "opus" ? openay::CodecType::Opus : openay::CodecType::Pcm;
    openay::TestSource source(freq);  // 440 Hz default, 480-sample frames
    openay::CapturePipeline pipe;
    if (!pipe.Configure(&source, openay::TransportType::Udp, host, port, ct,
                        10)) {
        fprintf(stderr, "openay: tone-udp: configure failed: %s\n",
                openay::PipelineErrorName(pipe.last_error()));
        return 1;
    }
    if (!pipe.Start()) {
        fprintf(stderr, "openay: tone-udp: start failed: %s\n",
                openay::PipelineErrorName(pipe.last_error()));
        return 1;
    }
    std::this_thread::sleep_for(
        std::chrono::milliseconds(static_cast<int>(seconds * 1000.0)));
    pipe.Stop();  // source stop -> drain -> join -> close sink

    const uint64_t packets = pipe.packets_sent();
    const uint64_t overruns = pipe.ring_overruns();
    const uint64_t send_errors = pipe.send_errors();
    printf("TONE seconds=%.1f packets=%llu overruns=%llu send_errors=%llu\n",
           seconds, static_cast<unsigned long long>(packets),
           static_cast<unsigned long long>(overruns),
           static_cast<unsigned long long>(send_errors));
    const bool healthy = pipe.Healthy() && overruns == 0 && send_errors == 0 &&
                         pipe.encode_errors() == 0;
    return healthy ? 0 : 1;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 2) {
        Usage(stderr);
        return 2;
    }
    const std::string cmd = argv[1];
    if (cmd == "-h" || cmd == "--help") {
        Usage(stdout);
        return 0;
    }

    if (cmd == "send-udp") {
        if (argc < 5 || argc > 7) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        size_t count = 0, payload = 480, interval = 0;
        if (!ParseU16(argv[3], &port) || !ParseCount(argv[4], &count) ||
            (argc >= 6 && !ParseSize(argv[5], &payload)) ||
            (argc >= 7 && !ParseCount(argv[6], &interval))) {
            Usage(stderr);
            return 2;
        }
        return SendUdp(argv[2], port, count, payload, interval);
    }
    if (cmd == "send-tcp") {
        if (argc < 5 || argc > 6) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        size_t count = 0, payload = 480;
        if (!ParseU16(argv[3], &port) || !ParseCount(argv[4], &count) ||
            (argc >= 6 && !ParseSize(argv[5], &payload))) {
            Usage(stderr);
            return 2;
        }
        return SendTcp(argv[2], port, count, payload);
    }
    if (cmd == "recv-udp") {
        if (argc < 4 || argc > 5) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        size_t count = 0, payload = 480;
        if (!ParseU16(argv[2], &port) || !ParseCount(argv[3], &count) ||
            (argc >= 5 && !ParseSize(argv[4], &payload))) {
            Usage(stderr);
            return 2;
        }
        return RecvUdp(port, count, payload);
    }
    if (cmd == "recv-tcp") {
        if (argc < 4 || argc > 5) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        size_t count = 0, payload = 480;
        if (!ParseU16(argv[2], &port) || !ParseCount(argv[3], &count) ||
            (argc >= 5 && !ParseSize(argv[4], &payload))) {
            Usage(stderr);
            return 2;
        }
        return RecvTcp(port, count, payload);
    }
    if (cmd == "bench") {
        if (argc < 5 || argc > 6) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        size_t count = 0, payload = 480;
        if (!ParseU16(argv[3], &port) || !ParseCount(argv[4], &count) ||
            (argc >= 6 && !ParseSize(argv[5], &payload))) {
            Usage(stderr);
            return 2;
        }
        return Bench(argv[2], port, count, payload);
    }
    if (cmd == "tone-udp") {
        if (argc < 5 || argc > 7) {
            Usage(stderr);
            return 2;
        }
        uint16_t port = 0;
        double seconds = 0.0, freq = 440.0;
        if (!ParseU16(argv[3], &port) || !ParseDouble(argv[4], &seconds) ||
            (argc >= 6 && !ParseDouble(argv[5], &freq))) {
            Usage(stderr);
            return 2;
        }
        const std::string codec = argc >= 7 ? argv[6] : "pcm";
        if (codec != "pcm" && codec != "opus") {
            fprintf(stderr, "openay: tone-udp: unknown codec '%s' (use pcm|opus)\n",
                    codec.c_str());
            return 2;
        }
        return ToneUdp(argv[2], port, seconds, freq, codec);
    }

    fprintf(stderr, "openay_loopback: unknown command '%s'\n", cmd.c_str());
    Usage(stderr);
    return 2;
}
