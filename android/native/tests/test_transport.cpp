// OpenAY Mic — transport unit tests.
//
// Covers: UDP loopback, TCP loopback, SeqTracker classification, malformed
// UDP datagram handling, and TCP stream resync.
#include "openay/protocol.h"
#include "openay/stats.h"
#include "openay/transport.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <mutex>
#include <thread>
#include <vector>

using openay::DecodeError;
using openay::EncodePacket;
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

int g_failures = 0;

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                \
        }                                                                \
    } while (0)

uint16_t PickPort() {
    // Use the 42000–42999 range so the parallel agent's 413xx is untouched.
    return static_cast<uint16_t>(42000 + (static_cast<uint16_t>(::getpid()) % 1000));
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

// ---------------------------------------------------------------------------
// SeqTracker unit tests
// ---------------------------------------------------------------------------

void TestSeqTracker() {
    uint16_t gap = 0;

    // In-order sequence.
    SeqTracker t1;
    CHECK(t1.Update(0, &gap) == SeqEvent::InOrder);
    for (uint16_t s = 1; s <= 1000; ++s) {
        CHECK(t1.Update(s, &gap) == SeqEvent::InOrder);
    }

    // Gap: expected 11, received 15 -> forward 4.
    SeqTracker t2;
    CHECK(t2.Update(10, &gap) == SeqEvent::InOrder);
    CHECK(t2.Update(15, &gap) == SeqEvent::Gap && gap == 4);
    CHECK(t2.Update(16, &gap) == SeqEvent::InOrder);

    // Duplicate.
    SeqTracker t3;
    CHECK(t3.Update(0, &gap) == SeqEvent::InOrder);
    CHECK(t3.Update(0, &gap) == SeqEvent::Duplicate);

    // Reorder: after 0,1,2, seq 1 is reorder.
    SeqTracker t4;
    CHECK(t4.Update(0, &gap) == SeqEvent::InOrder);
    CHECK(t4.Update(1, &gap) == SeqEvent::InOrder);
    CHECK(t4.Update(2, &gap) == SeqEvent::InOrder);
    CHECK(t4.Update(1, &gap) == SeqEvent::Reorder);

    // u16 wraparound: 0xFFFE, 0xFFFF, 0x0000, 0x0001 all InOrder.
    SeqTracker t5;
    CHECK(t5.Update(0xFFFE, &gap) == SeqEvent::InOrder);
    CHECK(t5.Update(0xFFFF, &gap) == SeqEvent::InOrder);
    CHECK(t5.Update(0x0000, &gap) == SeqEvent::InOrder);
    CHECK(t5.Update(0x0001, &gap) == SeqEvent::InOrder);

    // Gap across wrap: after 0xFFFF expected 0x0000; seq 0x0003 -> forward 3.
    SeqTracker t6;
    CHECK(t6.Update(0xFFFF, &gap) == SeqEvent::InOrder);
    CHECK(t6.Update(0x0003, &gap) == SeqEvent::Gap && gap == 3);

    // Reorder across wrap: after 0x0001 expected 0x0002; 0xFFFF -> backward.
    SeqTracker t7;
    CHECK(t7.Update(0xFFFE, &gap) == SeqEvent::InOrder);
    CHECK(t7.Update(0xFFFF, &gap) == SeqEvent::InOrder);
    CHECK(t7.Update(0x0000, &gap) == SeqEvent::InOrder);
    CHECK(t7.Update(0x0001, &gap) == SeqEvent::InOrder);
    CHECK(t7.Update(0xFFFF, &gap) == SeqEvent::Reorder);

    // Duplicate across wrap.
    SeqTracker t8;
    CHECK(t8.Update(0xFFFF, &gap) == SeqEvent::InOrder);
    CHECK(t8.Update(0xFFFF, &gap) == SeqEvent::Duplicate);

    // nullptr gap_count is tolerated.
    SeqTracker t9;
    CHECK(t9.Update(0, nullptr) == SeqEvent::InOrder);
    CHECK(t9.Update(2, nullptr) == SeqEvent::Gap);
}

// ---------------------------------------------------------------------------
// UDP loopback
// ---------------------------------------------------------------------------

void TestUdpLoopback() {
    const size_t kCount = 1000;
    const size_t kPayload = 480;
    const uint16_t port = PickPort();

    std::mutex mu;
    std::condition_variable cv;
    std::atomic<bool> bound{false};
    std::atomic<bool> recv_done{false};
    std::vector<Packet> got;
    PacketStats stats;

    std::thread th([&]() {
        UdpReceiver recv(port);
        if (!recv.Bind()) {
            bound = true;
            cv.notify_all();
            return;
        }
        bound = true;
        cv.notify_all();
        for (size_t i = 0; i < kCount; ++i) {
            Packet pkt;
            if (!recv.Recv(&pkt, 5000)) break;
            std::lock_guard<std::mutex> lk(mu);
            got.push_back(std::move(pkt));
        }
        stats = recv.stats();
        recv_done = true;
        cv.notify_all();
    });

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return bound.load(); });
    }

    UdpSender sender("127.0.0.1", port);
    CHECK(sender.Valid());
    std::vector<uint8_t> scratch(kPayload);
    for (size_t i = 0; i < kCount; ++i) {
        Packet pkt;
        pkt.type = (i % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
        pkt.seq = static_cast<uint16_t>(i & 0xFFFFu);
        FillXorshift(pkt.seq, scratch.data(), scratch.size());
        pkt.payload = scratch;
        CHECK(sender.Send(pkt));
    }

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return recv_done.load(); });
    }
    th.join();

    CHECK(got.size() == kCount);
    for (size_t i = 0; i < got.size(); ++i) {
        CHECK(got[i].seq == static_cast<uint16_t>(i));
        CHECK(got[i].type ==
              (i % 2 == 0 ? PayloadType::Pcm : PayloadType::Opus));
        CHECK(got[i].payload.size() == kPayload);
        CHECK(VerifyXorshift(static_cast<uint32_t>(i), got[i].payload.data(),
                             got[i].payload.size()));
    }
    CHECK(stats.received == kCount);
    CHECK(stats.lost == 0);
    CHECK(stats.duplicate == 0);
    CHECK(stats.out_of_order == 0);
    CHECK(stats.malformed == 0);
}

// ---------------------------------------------------------------------------
// TCP loopback
// ---------------------------------------------------------------------------

void TestTcpLoopback() {
    const size_t kCount = 1000;
    const size_t kPayload = 480;
    const uint16_t port = PickPort() + 10;

    std::mutex mu;
    std::condition_variable cv;
    std::atomic<bool> ready{false};
    std::atomic<bool> recv_done{false};
    std::vector<Packet> got;
    PacketStats stats;

    std::thread th([&]() {
        TcpServer server(port);
        if (!server.Listen()) {
            ready = true;
            cv.notify_all();
            return;
        }
        ready = true;
        cv.notify_all();
        std::unique_ptr<TcpConn> conn = server.Accept(5000);
        if (!conn) {
            recv_done = true;
            cv.notify_all();
            return;
        }
        for (size_t i = 0; i < kCount; ++i) {
            Packet pkt;
            if (!conn->Recv(&pkt, 5000)) break;
            std::lock_guard<std::mutex> lk(mu);
            got.push_back(std::move(pkt));
        }
        stats = conn->stats();
        recv_done = true;
        cv.notify_all();
    });

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return ready.load(); });
    }

    TcpClient client("127.0.0.1", port);
    CHECK(client.Valid());
    std::vector<uint8_t> scratch(kPayload);
    for (size_t i = 0; i < kCount; ++i) {
        Packet pkt;
        pkt.type = (i % 2 == 0) ? PayloadType::Pcm : PayloadType::Opus;
        pkt.seq = static_cast<uint16_t>(i & 0xFFFFu);
        FillXorshift(pkt.seq, scratch.data(), scratch.size());
        pkt.payload = scratch;
        CHECK(client.Send(pkt));
    }

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return recv_done.load(); });
    }
    th.join();

    CHECK(got.size() == kCount);
    for (size_t i = 0; i < got.size(); ++i) {
        CHECK(got[i].seq == static_cast<uint16_t>(i));
        CHECK(got[i].type ==
              (i % 2 == 0 ? PayloadType::Pcm : PayloadType::Opus));
        CHECK(got[i].payload.size() == kPayload);
        CHECK(VerifyXorshift(static_cast<uint32_t>(i), got[i].payload.data(),
                             got[i].payload.size()));
    }
    CHECK(stats.received == kCount);
    CHECK(stats.lost == 0);
    CHECK(stats.duplicate == 0);
    CHECK(stats.out_of_order == 0);
    CHECK(stats.malformed == 0);
}

// ---------------------------------------------------------------------------
// Malformed UDP datagram: counted, receiver survives.
// ---------------------------------------------------------------------------

void TestMalformedUdp() {
    const uint16_t port = PickPort() + 20;
    std::mutex mu;
    std::condition_variable cv;
    std::atomic<bool> bound{false};
    std::atomic<bool> got_packet{false};
    PacketStats stats;

    std::thread th([&]() {
        UdpReceiver recv(port);
        if (!recv.Bind()) {
            bound = true;
            cv.notify_all();
            return;
        }
        bound = true;
        cv.notify_all();
        for (int i = 0; i < 100; ++i) {
            Packet pkt;
            if (recv.Recv(&pkt, 2000)) {
                got_packet = true;
                break;
            }
        }
        stats = recv.stats();
        cv.notify_all();
    });

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return bound.load(); });
    }

    // Send a truncated (3-byte) datagram.
    {
        const int fd = socket(AF_INET, SOCK_DGRAM, 0);
        CHECK(fd >= 0);
        sockaddr_in addr{};
        addr.sin_family = AF_INET;
        addr.sin_port = htons(port);
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        const uint8_t garbage[] = {0x00, 0x01, 0x02};
        sendto(fd, garbage, sizeof(garbage), 0,
               reinterpret_cast<const sockaddr*>(&addr), sizeof(addr));
        // bad-magic datagram (10 bytes, no 0xA7 at start).
        const uint8_t bad[] = {0x00, 0x00, 0x00, 0x01, 0x00, 0x04, 0xde, 0xad, 0xbe, 0xef};
        sendto(fd, bad, sizeof(bad), 0,
               reinterpret_cast<const sockaddr*>(&addr), sizeof(addr));
        close(fd);
    }

    // Send a valid packet.
    UdpSender sender("127.0.0.1", port);
    CHECK(sender.Valid());
    Packet pkt;
    pkt.type = PayloadType::Pcm;
    pkt.seq = 0;
    pkt.payload = {0x11, 0x22};
    CHECK(sender.Send(pkt));

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return got_packet.load(); });
    }
    th.join();

    CHECK(stats.received == 1);
    CHECK(stats.malformed == 2);
}

// ---------------------------------------------------------------------------
// TCP stream resync: garbage bytes followed by a valid packet.
// ---------------------------------------------------------------------------

void TestTcpResync() {
    const uint16_t port = PickPort() + 30;
    std::mutex mu;
    std::condition_variable cv;
    std::atomic<bool> ready{false};
    std::atomic<bool> done{false};
    std::vector<Packet> got;
    PacketStats stats;

    std::thread th([&]() {
        TcpServer server(port);
        if (!server.Listen()) {
            ready = true;
            cv.notify_all();
            return;
        }
        ready = true;
        cv.notify_all();
        std::unique_ptr<TcpConn> conn = server.Accept(5000);
        if (!conn) {
            done = true;
            cv.notify_all();
            return;
        }
        for (int i = 0; i < 5; ++i) {
            Packet pkt;
            if (!conn->Recv(&pkt, 5000)) break;
            std::lock_guard<std::mutex> lk(mu);
            got.push_back(std::move(pkt));
        }
        stats = conn->stats();
        done = true;
        cv.notify_all();
    });

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return ready.load(); });
    }

    // Open a raw TCP socket and send garbage, then a valid packet.
    const int fd = socket(AF_INET, SOCK_STREAM, 0);
    CHECK(fd >= 0);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_port = htons(port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    CHECK(connect(fd, reinterpret_cast<const sockaddr*>(&addr), sizeof(addr)) == 0);

    // Garbage: 12 bytes that contain no 0xA7.
    const char garbage[] = "JUNKJUNKJUNK";
    CHECK(write(fd, garbage, strlen(garbage)) == static_cast<ssize_t>(strlen(garbage)));

    // Valid packet.
    Packet pkt;
    pkt.type = PayloadType::Pcm;
    pkt.seq = 7;
    pkt.payload = {0x01, 0x02, 0x03};
    const std::vector<uint8_t> wire = EncodePacket(pkt);
    CHECK(write(fd, wire.data(), wire.size()) == static_cast<ssize_t>(wire.size()));
    close(fd);

    {
        std::unique_lock<std::mutex> lk(mu);
        cv.wait(lk, [&]() { return done.load(); });
    }
    th.join();

    CHECK(got.size() == 1);
    if (!got.empty()) {
        CHECK(got[0].seq == 7);
        CHECK(got[0].type == PayloadType::Pcm);
        CHECK(got[0].payload.size() == 3);
        CHECK(got[0].payload[0] == 0x01);
        CHECK(got[0].payload[1] == 0x02);
        CHECK(got[0].payload[2] == 0x03);
    }
    CHECK(stats.malformed >= 1);
}

}  // namespace

int main() {
    TestSeqTracker();
    TestUdpLoopback();
    TestTcpLoopback();
    TestMalformedUdp();
    TestTcpResync();
    if (g_failures == 0) {
        printf("test_transport: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "test_transport: %d check(s) FAILED\n", g_failures);
    return 1;
}