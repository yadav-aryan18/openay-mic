// OpenAY Mic — Bluetooth fd-bridge tests (host-only; no phone/Bluetooth stack).
//
// The Android Bluetooth transport is a pure fd seam: Kotlin owns the RFCOMM
// (SPP) socket, hands native the write end of a pipe, and a pump coroutine
// forwards bytes into the socket. Native writes TCP-identical framing into
// that fd (FdBytePipe wrapped by the pipeline's fd sink). A host socketpair
// or pipe stands in for the Kotlin side:
//
//   * FdBytePipe packet loopback (socketpair): 1000 hand-built packets pushed
//     as wire bytes and parsed off the peer — seq/type/payload plus xorshift
//     payload continuity, exactly like the TCP loopback test.
//   * FdBytePipe byte fidelity (pipe): 4 MiB of xorshift through the
//     write() fallback path (non-socket fd, as in production), byte-exact.
//   * FdBytePipe closed peer: Push returns false after the peer closed (the
//     MSG_NOSIGNAL path keeps SIGPIPE from killing the process).
//   * CapturePipeline.ConfigureBt + TestSource: ~2 s stream parsed off the
//     peer — seq contiguous from 0, 960-byte PCM payloads, packet rate in
//     the 10 ms window, sine RMS > 1000, and sample-exact sine continuity
//     (every payload matches TestSource's phase; any lost/garbled byte or
//     dropped frame breaks it).
//   * Engine facade: ConfigureBt + StatsJson shape ("transport":"bt",
//     device address as "host").
//   * Failure paths: a negative fd rejects Configure with invalid_config, a
//     bad frame_ms likewise, and a failed configure consumes (closes) the
//     handed-over fd — the ownership contract, proven via EBADF.
#include "openay/capture_pipeline.h"
#include "openay/capture_engine.h"
#include "openay/protocol.h"
#include "openay/test_source.h"
#include "openay/transport.h"

#include <poll.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

using openay::CaptureEngine;
using openay::CapturePipeline;
using openay::CodecType;
using openay::DecodeError;
using openay::EncodePacket;
using openay::FdBytePipe;
using openay::HeaderPayloadLength;
using openay::kHeaderLen;
using openay::LoadU16BE;
using openay::Packet;
using openay::PayloadType;
using openay::PipelineError;
using openay::TestSource;

namespace {

int g_failures = 0;

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                \
        }                                                                \
    } while (0)

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

// Sample-level helper on little-endian 16-bit PCM payloads (as test_capture).
inline int16_t SampleAt(const uint8_t* b, size_t i) {
    return static_cast<int16_t>(static_cast<uint16_t>(b[2 * i]) |
                                (static_cast<uint16_t>(b[2 * i + 1]) << 8));
}

// ---------------------------------------------------------------------------
// StreamParser — incremental decoder for the 6-byte-header byte stream (the
// reader side of the seam; mirrors the desktop's TCP framing rules: validate
// the header, read exactly payload_len more bytes).
// ---------------------------------------------------------------------------

struct StreamParser {
    uint64_t malformed = 0;

    // Decodes every complete packet in `data` and appends it to `out`. A bad
    // header drops one byte and rescans (TCP-style resync — a healthy run
    // must never hit this; the counter is asserted to stay 0).
    void Feed(const uint8_t* data, size_t n, std::vector<Packet>* out) {
        buf_.insert(buf_.end(), data, data + n);
        size_t off = 0;
        while (buf_.size() - off >= kHeaderLen) {
            const uint8_t* hdr = buf_.data() + off;
            uint16_t plen = 0;
            DecodeError err = DecodeError::BadMagic;
            if (!HeaderPayloadLength(hdr, &plen, &err)) {
                ++malformed;
                ++off;
                continue;
            }
            if (buf_.size() - off < kHeaderLen + plen) break;  // need payload
            Packet p;
            p.type = static_cast<PayloadType>(hdr[1]);
            p.seq = LoadU16BE(hdr + 2);
            p.payload.assign(hdr + kHeaderLen, hdr + kHeaderLen + plen);
            out->push_back(std::move(p));
            off += kHeaderLen + plen;
        }
        buf_.erase(buf_.begin(),
                   buf_.begin() + static_cast<std::ptrdiff_t>(off));
    }

    std::vector<uint8_t> buf_;
};

// Reads the peer end until EOF (native closed its fd — the clean-stop
// terminator), or until `stop` plus a 1 s idle guard, feeding the parser.
// A 15 s wall-clock guard keeps a hung writer from wedging ctest.
void RunReader(int fd, std::atomic<bool>* stop, StreamParser* parser,
               std::vector<Packet>* out) {
    uint8_t buf[4096];
    int idle_ms = 0;
    const auto deadline = std::chrono::steady_clock::now() +
                          std::chrono::seconds(15);
    for (;;) {
        pollfd pfd{fd, POLLIN, 0};
        const int prc = poll(&pfd, 1, 200);
        if (prc < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (prc == 0) {
            if (stop->load(std::memory_order_relaxed)) {
                idle_ms += 200;
                if (idle_ms >= 1000) break;  // idle after stop: stream finished
            }
            if (std::chrono::steady_clock::now() > deadline) break;
            continue;
        }
        const ssize_t r = read(fd, buf, sizeof(buf));
        if (r == 0) break;  // EOF: the write end was closed
        if (r < 0) {
            if (errno == EINTR) continue;
            break;
        }
        parser->Feed(buf, static_cast<size_t>(r), out);
        idle_ms = 0;
    }
}

// ---------------------------------------------------------------------------
// FdBytePipe packet loopback over a socketpair (the MSG_NOSIGNAL send path).
// ---------------------------------------------------------------------------

void TestFdPipePacketLoopback() {
    const size_t kCount = 1000;
    const size_t kPayload = 480;

    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sv[0] < 0 || sv[1] < 0) return;

    StreamParser parser;
    std::vector<Packet> got;
    std::atomic<bool> stop{false};
    std::thread reader(RunReader, sv[1], &stop, &parser, &got);

    {
        FdBytePipe pipe(sv[0]);  // owns sv[0]; closed at scope exit (EOF)
        CHECK(pipe.Valid());
        std::vector<uint8_t> scratch(kPayload);
        for (size_t i = 0; i < kCount; ++i) {
            Packet pkt;
            pkt.type = (i % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
            pkt.seq = static_cast<uint16_t>(i & 0xFFFFu);
            FillXorshift(pkt.seq, scratch.data(), scratch.size());
            pkt.payload = scratch;
            const std::vector<uint8_t> wire = EncodePacket(pkt);
            CHECK(pipe.Push(wire.data(), wire.size()));
        }
    }

    stop.store(true, std::memory_order_relaxed);
    reader.join();
    close(sv[1]);

    CHECK(got.size() == kCount);
    for (size_t i = 0; i < got.size(); ++i) {
        CHECK(got[i].seq == static_cast<uint16_t>(i));
        CHECK(got[i].type ==
              (i % 2 == 0 ? PayloadType::Pcm : PayloadType::Opus));
        CHECK(got[i].payload.size() == kPayload);
        CHECK(VerifyXorshift(static_cast<uint32_t>(i), got[i].payload.data(),
                             got[i].payload.size()));
    }
    CHECK(parser.malformed == 0);
}

// ---------------------------------------------------------------------------
// FdBytePipe byte fidelity over a real pipe (the production fd kind): forces
// short writes through the write() fallback with 4 MiB of xorshift bytes.
// ---------------------------------------------------------------------------

void TestFdPipeByteFidelity() {
    int fds[2];
    CHECK(pipe(fds) == 0);
    if (fds[0] < 0 || fds[1] < 0) return;

    constexpr size_t kTotal = 4u << 20;  // 4 MiB >> the 64 KiB pipe buffer
    std::vector<uint8_t> tx(kTotal);
    FillXorshift(0xC0FFEEu, tx.data(), tx.size());

    std::vector<uint8_t> rx;
    std::thread reader([&]() {
        uint8_t buf[8192];
        const auto deadline = std::chrono::steady_clock::now() +
                              std::chrono::seconds(15);
        for (;;) {
            pollfd pfd{fds[0], POLLIN, 0};
            const int prc = poll(&pfd, 1, 200);
            if (prc < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (prc == 0) {
                if (std::chrono::steady_clock::now() > deadline) break;
                continue;
            }
            const ssize_t r = read(fds[0], buf, sizeof(buf));
            if (r == 0) break;  // write end closed
            if (r < 0) {
                if (errno == EINTR) continue;
                break;
            }
            rx.insert(rx.end(), buf, buf + r);
        }
    });

    {
        FdBytePipe pipe(fds[1]);  // non-socket: flips to write() on first send
        CHECK(pipe.Valid());
        size_t off = 0;
        while (off < kTotal) {
            const size_t n = std::min<size_t>(256 * 1024, kTotal - off);
            CHECK(pipe.Push(tx.data() + off, n));
            off += n;
        }
    }

    reader.join();
    close(fds[0]);

    CHECK(rx.size() == kTotal);
    if (rx.size() == kTotal) {
        CHECK(std::memcmp(rx.data(), tx.data(), kTotal) == 0);
    }
}

// ---------------------------------------------------------------------------
// Closed peer: Push must fail gracefully (false), not die by SIGPIPE.
// ---------------------------------------------------------------------------

void TestFdPipeClosedPeer() {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sv[0] < 0 || sv[1] < 0) return;
    close(sv[1]);  // peer gone before any write

    FdBytePipe pipe(sv[0]);
    const uint8_t b[4] = {1, 2, 3, 4};
    CHECK(!pipe.Push(b, sizeof(b)));  // EPIPE; MSG_NOSIGNAL keeps SIGPIPE away
    // FdBytePipe's destructor closes sv[0]; no manual close here.
}

// ---------------------------------------------------------------------------
// TestSource's exact sample law (test_source.cpp): the sine starts at phase 0
// and advances one 480-sample frame per packet, so every payload sample is
// predictable from the packet's seq. A ±2 tolerance absorbs libm/FMA drift
// between TUs; any lost or garbled byte breaks it by far more.
// ---------------------------------------------------------------------------

int16_t ExpectedSine(uint16_t seq, size_t i) {
    const double amplitude = 0.4 * 32767.0;
    const double two_pi_f = 2.0 * M_PI * 440.0;
    const double t =
        static_cast<double>(static_cast<size_t>(seq) * 480 + i) / 48000.0;
    return static_cast<int16_t>(amplitude * std::sin(two_pi_f * t));
}

// The required end-to-end case: TestSource -> CapturePipeline.ConfigureBt ->
// fd sink -> socketpair -> parsed + verified off the peer.
void TestBtPipeline() {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sv[0] < 0 || sv[1] < 0) return;

    TestSource source;  // 440 Hz, 0.4 * 32767, 480-sample frames
    CapturePipeline pipe;
    // 1 MiB ring (~1070 frames of slack): a scheduler stall on a loaded CI
    // runner must not turn into ring overruns — the test asserts delivery
    // and fidelity, not host timing luck.
    const bool configured =
        pipe.ConfigureBt(&source, sv[0], CodecType::Pcm, 10, 1 << 20);
    CHECK(configured);
    if (!configured) {
        // The handed-over fd is consumed even on failure (ownership rule).
        close(sv[1]);
        return;
    }
    CHECK(pipe.Start());

    StreamParser parser;
    std::vector<Packet> pkts;
    std::atomic<bool> stop{false};
    std::thread recv_thread(RunReader, sv[1], &stop, &parser, &pkts);

    std::this_thread::sleep_for(std::chrono::milliseconds(2000));
    pipe.Stop();  // source stop -> drain -> join -> close the fd (EOF)
    stop.store(true, std::memory_order_relaxed);
    recv_thread.join();
    close(sv[1]);

    printf("capture_bt: received=%zu (%.1f pkt/s)\n", pkts.size(),
           static_cast<double>(pkts.size()) / 2.0);
    // ~200 expected at 10 ms frames; the wide lower bound tolerates a ~0.6 s
    // scheduler stall without failing delivery (see the 1 MiB ring above).
    CHECK(pkts.size() >= 140 && pkts.size() <= 230);
    CHECK(parser.malformed == 0);

    size_t bad_seq = 0, bad_type = 0, bad_size = 0, sine_mismatch = 0;
    double sq = 0.0;
    size_t samples = 0;
    for (size_t i = 0; i < pkts.size(); ++i) {
        if (pkts[i].seq != static_cast<uint16_t>(i)) ++bad_seq;
        if (pkts[i].type != PayloadType::Pcm) ++bad_type;
        if (pkts[i].payload.size() != 960) ++bad_size;
        for (size_t j = 0; j < pkts[i].payload.size() / 2; ++j) {
            const int16_t got = SampleAt(pkts[i].payload.data(), j);
            const int16_t want = ExpectedSine(pkts[i].seq, j);
            sq += static_cast<double>(got) * got;
            ++samples;
            if (std::abs(static_cast<int>(got) - static_cast<int>(want)) > 2) {
                ++sine_mismatch;
            }
        }
    }
    const double rms = samples > 0 ? std::sqrt(sq / static_cast<double>(samples))
                                   : 0.0;
    printf("capture_bt: rms=%.1f sine_mismatches=%zu\n", rms, sine_mismatch);
    CHECK(bad_seq == 0);   // contiguous from 0 (no dropped/reordered frames)
    CHECK(bad_type == 0);
    CHECK(bad_size == 0);
    CHECK(sine_mismatch == 0);  // sample-exact continuity: no byte loss
    CHECK(rms > 1000.0);
    CHECK(pipe.ring_overruns() == 0);
    CHECK(pipe.encode_errors() == 0);
    CHECK(pipe.send_errors() == 0);
    CHECK(pipe.Healthy());
    CHECK(pipe.packets_sent() == pkts.size());
}

// ---------------------------------------------------------------------------
// Engine facade over the fd bridge: ConfigureBt/Start/Stop + StatsJson shape.
// ---------------------------------------------------------------------------

void TestBtEngineSmoke() {
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sv[0] < 0 || sv[1] < 0) return;

    StreamParser parser;
    std::vector<Packet> pkts;
    std::atomic<bool> stop{false};
    std::thread reader(RunReader, sv[1], &stop, &parser, &pkts);

    CaptureEngine engine;
    CHECK(engine.ConfigureBt(sv[0], "AA:BB:CC:DD:EE:FF", CodecType::Pcm, 10));
    CHECK(engine.Start());
    CHECK(engine.IsRunning());
    std::this_thread::sleep_for(std::chrono::milliseconds(400));
    const std::string during = engine.StatsJson();
    printf("capture_bt: engine stats during run:\n%s\n", during.c_str());
    CHECK(during.find("\"running\":true") != std::string::npos);
    CHECK(during.find("\"transport\":\"bt\"") != std::string::npos);
    CHECK(during.find("\"host\":\"AA:BB:CC:DD:EE:FF\"") != std::string::npos);
    CHECK(during.find("\"port\":0") != std::string::npos);

    engine.Stop();  // closes the fd -> reader sees EOF
    CHECK(!engine.IsRunning());
    stop.store(true, std::memory_order_relaxed);
    reader.join();
    close(sv[1]);

    const std::string after = engine.StatsJson();
    CHECK(after.find("\"running\":false") != std::string::npos);
    CHECK(after.find("\"transport\":\"bt\"") != std::string::npos);
    CHECK(parser.malformed == 0);
    CHECK(pkts.size() > 0);  // the smoke reader saw real framed traffic
}

// ---------------------------------------------------------------------------
// Failure paths + the fd ownership contract.
// ---------------------------------------------------------------------------

void TestBtInvalidConfig() {
    TestSource source;
    CapturePipeline pipe;

    // Negative fd: nothing was handed over -> invalid_config.
    CHECK(!pipe.ConfigureBt(&source, -1, CodecType::Pcm, 10));
    CHECK(pipe.last_error() == PipelineError::InvalidConfig);

    // Bad frame size with a real fd: invalid_config, and the fd is consumed
    // (closed) by the failed configure — proven with EBADF on the writer end.
    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    if (sv[0] >= 0 && sv[1] >= 0) {
        CHECK(!pipe.ConfigureBt(&source, sv[0], CodecType::Pcm, 7));
        CHECK(pipe.last_error() == PipelineError::InvalidConfig);
        CHECK(write(sv[0], "x", 1) < 0 && errno == EBADF);
        close(sv[1]);
    }

    CaptureEngine engine;
    CHECK(!engine.ConfigureBt(-1, "AA:BB:CC:DD:EE:FF", CodecType::Pcm, 10));
    const std::string json = engine.StatsJson();
    CHECK(json.find("\"last_error\":\"invalid_config\"") != std::string::npos);
    CHECK(json.find("\"transport\":\"bt\"") != std::string::npos);
}

}  // namespace

int main() {
    TestFdPipePacketLoopback();
    TestFdPipeByteFidelity();
    TestFdPipeClosedPeer();
    TestBtPipeline();
    TestBtEngineSmoke();
    TestBtInvalidConfig();
    if (g_failures == 0) {
        printf("test_capture_bt: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "test_capture_bt: %d check(s) FAILED\n", g_failures);
    return 1;
}
