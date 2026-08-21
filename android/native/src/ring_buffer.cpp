// Lock-free SPSC ring buffer implementation (see ring_buffer.h).
#include "openay/ring_buffer.h"

#include <cstring>

namespace openay {
namespace {

// Round up to the next power of two (min 2); the SPSC algorithm needs a mask.
size_t RoundUpPow2(size_t capacity) {
    size_t cap = 2;
    while (cap < capacity) cap <<= 1;
    return cap;
}

}  // namespace

SpscRingBuffer::SpscRingBuffer(size_t capacity)
    : capacity_(RoundUpPow2(capacity)),
      mask_(capacity_ - 1),
      data_(capacity_) {}

size_t SpscRingBuffer::Push(const uint8_t* data, size_t len) {
    if (len == 0) return 0;
    // Single producer: head_ needs only relaxed access. tail_ is loaded with
    // acquire so this producer sees the consumer's released free space.
    const size_t head = head_.load(std::memory_order_relaxed);
    const size_t tail = tail_.load(std::memory_order_acquire);
    const size_t used = head - tail;
    const size_t free_space = capacity_ - 1 - used;  // keep one byte of slack
    if (len > free_space) {
        // Drop-whole-block policy (documented in ring_buffer.h): a partial
        // write would hand the consumer a torn frame.
        overruns_.fetch_add(1, std::memory_order_relaxed);
        return 0;
    }
    const size_t first = Index(head);
    if (first + len <= capacity_) {
        std::memcpy(data_.data() + first, data, len);
    } else {
        const size_t a = capacity_ - first;
        std::memcpy(data_.data() + first, data, a);
        std::memcpy(data_.data(), data + a, len - a);
    }
    // Release: publishes the payload writes to the consumer's acquire load.
    head_.store(head + len, std::memory_order_release);
    return len;
}

size_t SpscRingBuffer::Pop(uint8_t* dst, size_t maxlen) {
    if (maxlen == 0) return 0;
    // Single consumer: tail_ needs only relaxed access. head_ is loaded with
    // acquire so payload bytes released by the producer are visible here.
    const size_t tail = tail_.load(std::memory_order_relaxed);
    const size_t head = head_.load(std::memory_order_acquire);
    const size_t avail = head - tail;
    const size_t n = avail < maxlen ? avail : maxlen;
    if (n == 0) return 0;
    const size_t first = Index(tail);
    if (first + n <= capacity_) {
        std::memcpy(dst, data_.data() + first, n);
    } else {
        const size_t a = capacity_ - first;
        std::memcpy(dst, data_.data() + first, a);
        std::memcpy(dst + a, data_.data(), n - a);
    }
    // Release: publishes the freed space to the producer's acquire load.
    tail_.store(tail + n, std::memory_order_release);
    return n;
}

size_t SpscRingBuffer::Available() const {
    const size_t head = head_.load(std::memory_order_acquire);
    const size_t tail = tail_.load(std::memory_order_relaxed);
    return head - tail;
}

}  // namespace openay
