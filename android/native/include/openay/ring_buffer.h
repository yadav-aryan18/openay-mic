// OpenAY Mic — single-producer/single-consumer lock-free ring buffer.
//
// Hard-RT contract (Phase 3 plan): the Oboe audio callback may ONLY do
// lock-free ring-buffer writes. This class is that buffer:
//
//   * Fixed power-of-two byte capacity, set at construction. The buffer makes
//     exactly one heap allocation in the constructor (the storage vector);
//     after construction there is NO heap allocation, NO lock, and NO syscall
//     on any operation.
//   * Single producer (the audio/RT callback thread) calls Push();
//     single consumer (the network thread) calls Pop()/Available(). head_
//     (producer) and tail_ (consumer) are std::atomic<size_t>; payload writes
//     are published with a release-store on head_ and observed with an
//     acquire-load on head_ by the consumer, so bytes are never visible
//     before the producer has finished writing them.
//   * Full-buffer policy: DROP-WHOLE-BLOCK. If `len` bytes cannot fit, nothing
//     is written, Push returns 0, and the internal overrun counter is
//     incremented. A partial write was rejected by design: it would hand the
//     consumer a torn frame (stale tail + new head) and break the
//     "every byte in the ring is a contiguous, correctly ordered slice of the
//     audio stream" invariant that frame-based Pop depends on. The caller
//     receives 0 and can additionally track the drop itself; the pipeline
//     exposes ring_overruns() straight from overruns().
//   * Capacity is a power of two so wrap-around is a mask, and the buffer
//     always keeps one byte of slack (capacity - 1 usable) so "full" and
//     "empty" can never be confused.
//
// The counters are monotonic (they wrap modulo 2^64, which unsigned
// arithmetic handles for free); capacity is far below 2^64 so
// head - tail is always the exact byte count in the buffer.
#ifndef OPENAY_RING_BUFFER_H
#define OPENAY_RING_BUFFER_H

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <vector>

namespace openay {

class SpscRingBuffer {
public:
    // Rounds `capacity` up to the next power of two (min 2). Usable space is
    // capacity - 1 (one byte of slack).
    explicit SpscRingBuffer(size_t capacity);

    // No heap allocation, no locking, no syscalls after construction.
    ~SpscRingBuffer() = default;
    SpscRingBuffer(const SpscRingBuffer&) = delete;
    SpscRingBuffer& operator=(const SpscRingBuffer&) = delete;

    // Producer only. All-or-nothing: returns `len` on success, 0 when the
    // whole block does not fit (drop-whole-block; counted as one overrun).
    size_t Push(const uint8_t* data, size_t len);

    // Consumer only. Drains up to `maxlen` bytes into dst; returns the number
    // of bytes copied (0 when the buffer is empty).
    size_t Pop(uint8_t* dst, size_t maxlen);

    // Consumer only (may also be read from stats paths; never the producer).
    size_t Available() const;

    // Total dropped whole blocks (producer-side counter, safe to read from
    // any thread).
    uint64_t overruns() const {
        return overruns_.load(std::memory_order_relaxed);
    }

    // Actual (power-of-two) byte capacity, including the slack byte.
    size_t capacity() const { return capacity_; }

private:
    size_t Index(size_t pos) const { return pos & mask_; }

    const size_t capacity_;  // power of two
    const size_t mask_;      // capacity_ - 1
    std::vector<uint8_t> data_;
    std::atomic<size_t> head_{0};   // producer write position (monotonic)
    std::atomic<size_t> tail_{0};   // consumer read position (monotonic)
    std::atomic<uint64_t> overruns_{0};
};

}  // namespace openay

#endif  // OPENAY_RING_BUFFER_H
