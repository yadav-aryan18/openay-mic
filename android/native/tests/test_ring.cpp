// OpenAY Mic — SPSC ring buffer stress tests.
//
// Test 1: Producer-consumer stress. A producer thread pushes xorshift-filled
// blocks of random size (1..960 bytes) for ~2 seconds while a consumer drains
// them and verifies byte-stream order and content integrity. The ring is sized
// generously (16 KiB) and the producer paces at one block per 100 µs so the
// consumer can keep up; the test asserts overruns() == 0.
//
// Test 2: Overrun + drop-whole-block semantics. Fill the ring completely,
// attempt one more push (must return 0 and increment overruns), then verify
// that prior contents pop out intact. Also tests partial-space drop: push
// into a nearly-full ring and assert the full block is dropped.
#include "openay/ring_buffer.h"

#include <algorithm>
#include <atomic>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <thread>
#include <vector>

using openay::SpscRingBuffer;

namespace {

std::atomic<int> g_failures{0};

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            g_failures.fetch_add(1, std::memory_order_relaxed);          \
        }                                                                \
    } while (0)

// Deterministic byte-at-a-position function: each byte of the global stream
// at position `i` is a pure function of `i`. The producer and consumer each
// compute it independently.
inline uint8_t ByteAt(uint64_t i) {
    uint32_t s = static_cast<uint32_t>((i * 0x9E3779B1u) ^ (i >> 32)) + 0x85EBCA6Bu;
    s ^= s << 13;
    s ^= s >> 17;
    s ^= s << 5;
    return static_cast<uint8_t>(s & 0xFFu);
}

// Assisted-storage xorshift (for random block sizes).
inline uint32_t XorShift32(uint32_t* state) {
    uint32_t s = *state;
    s ^= s << 13;
    s ^= s >> 17;
    s ^= s << 5;
    *state = s;
    return s;
}

// ---------------------------------------------------------------------------
// Test 1: SPSC stress with byte-stream integrity verification.
// ---------------------------------------------------------------------------

void TestStress() {
    constexpr size_t kCapacity = 16384;  // power of two
    SpscRingBuffer ring(kCapacity);

    std::atomic<bool> stop{false};
    std::atomic<uint64_t> written{0};
    uint64_t consumed = 0;

    // Producer: pushes blocks of random size 1..960, filled with ByteAt().
    std::thread producer([&]() {
        uint32_t rng = 0x12345678u;
        std::vector<uint8_t> block(960);
        uint64_t global = 0;
        while (!stop.load(std::memory_order_relaxed)) {
            const size_t len = 1 + (XorShift32(&rng) % 960);
            block.resize(len);
            for (size_t i = 0; i < len; ++i) {
                block[i] = ByteAt(global + i);
            }
            const size_t n = ring.Push(block.data(), len);
            if (n == 0) {
                // Overrun — should not happen with the pacing below.
                // The test asserts overruns() == 0 at the end.
                break;
            }
            global += n;
            written.store(global, std::memory_order_relaxed);
            // 100 µs pace: ~20k blocks in 2 s, ~9.6 MB total.
            std::this_thread::sleep_for(std::chrono::microseconds(100));
        }
    });

    // Consumer: drains and verifies every byte.
    std::thread consumer([&]() {
        uint8_t buf[4096];
        while (true) {
            const size_t n = ring.Pop(buf, sizeof(buf));
            if (n == 0) {
                if (stop.load(std::memory_order_relaxed) &&
                    ring.Available() == 0) {
                    break;
                }
                std::this_thread::yield();
                continue;
            }
            for (size_t i = 0; i < n; ++i) {
                const uint8_t expected = ByteAt(consumed + i);
                if (buf[i] != expected) {
                    fprintf(stderr,
                            "FAIL: byte mismatch at global position %llu "
                            "(got 0x%02x, expected 0x%02x)\n",
                            static_cast<unsigned long long>(consumed + i),
                            buf[i], expected);
                    g_failures.fetch_add(1, std::memory_order_relaxed);
                }
            }
            consumed += n;
        }
    });

    // Let the test run for ~2 seconds wall time.
    std::this_thread::sleep_for(std::chrono::milliseconds(2000));
    stop.store(true, std::memory_order_relaxed);
    producer.join();
    // Now the consumer sees stop == true and drains the remainder.
    consumer.join();

    const uint64_t total_written = written.load(std::memory_order_relaxed);
    printf("ring: stress written=%llu consumed=%llu overruns=%llu\n",
           static_cast<unsigned long long>(total_written),
           static_cast<unsigned long long>(consumed),
           static_cast<unsigned long long>(ring.overruns()));

    CHECK(total_written == consumed);
    CHECK(ring.overruns() == 0);
    CHECK(total_written > 10000);  // sanity: the test actually ran
}

// ---------------------------------------------------------------------------
// Test 2: Overrun and drop-whole-block semantics.
// ---------------------------------------------------------------------------

void TestOverrun() {
    // Capacity 8 -> usable 7 (one byte of slack).
    SpscRingBuffer ring(8);
    CHECK(ring.capacity() == 8);

    uint8_t buf[16];

    // Push 4 bytes -> fit.
    const uint8_t block1[] = {0x10, 0x20, 0x30, 0x40};
    CHECK(ring.Push(block1, 4) == 4);
    CHECK(ring.Available() == 4);

    // Push 8 bytes -> free space is 7-4 = 3 < 8 -> drop whole block.
    const uint8_t block2[] = {0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88};
    CHECK(ring.Push(block2, 8) == 0);
    CHECK(ring.overruns() == 1);  // one overrun counted
    CHECK(ring.Available() == 4); // block1 is still intact

    // Pop the original 4 bytes.
    std::memset(buf, 0xAA, sizeof(buf));
    CHECK(ring.Pop(buf, 4) == 4);
    CHECK(buf[0] == 0x10 && buf[1] == 0x20 && buf[2] == 0x30 && buf[3] == 0x40);
    CHECK(ring.Available() == 0);

    // Now push 3 bytes (fits: 0 + 3 <= 7) and pop them back.
    const uint8_t block3[] = {0xDE, 0xAD, 0xBE};
    CHECK(ring.Push(block3, 3) == 3);
    CHECK(ring.Available() == 3);
    std::memset(buf, 0xAA, sizeof(buf));
    CHECK(ring.Pop(buf, 3) == 3);
    CHECK(buf[0] == 0xDE && buf[1] == 0xAD && buf[2] == 0xBE);
    CHECK(ring.Available() == 0);

    // Fill completely: capacity 8 -> max usable 7.
    // Push 7 bytes.
    const uint8_t block4[] = {0, 1, 2, 3, 4, 5, 6};
    CHECK(ring.Push(block4, 7) == 7);
    CHECK(ring.Available() == 7);
    // One more push of any size (>=1) must fail.
    const uint8_t block5[] = {0xFF};
    CHECK(ring.Push(block5, 1) == 0);
    CHECK(ring.overruns() == 2);
    // The 7 bytes are still intact.
    CHECK(ring.Available() == 7);
    std::memset(buf, 0xAA, sizeof(buf));
    CHECK(ring.Pop(buf, 7) == 7);
    for (int i = 0; i < 7; ++i) CHECK(buf[i] == static_cast<uint8_t>(i));
    CHECK(ring.Available() == 0);

    printf("ring: overrun/block-drop tests passed (overruns=%llu)\n",
           static_cast<unsigned long long>(ring.overruns()));
}

}  // namespace

int main() {
    TestStress();
    TestOverrun();
    if (g_failures.load() == 0) {
        printf("test_ring: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "test_ring: %d check(s) FAILED\n", g_failures.load());
    return 1;
}