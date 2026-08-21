// Real-time sine generator implementation (see test_source.h).
#include "openay/test_source.h"

#include <chrono>
#include <cmath>
#include <vector>

namespace openay {

TestSource::TestSource(double freq_hz, double amplitude, size_t frame_samples)
    : freq_hz_(freq_hz), amplitude_(amplitude), frame_samples_(frame_samples) {}

bool TestSource::Start() {
    if (running_.load(std::memory_order_relaxed)) return true;  // idempotent
    running_.store(true, std::memory_order_relaxed);
    thread_ = std::thread(&TestSource::Run, this);
    return true;
}

void TestSource::Stop() {
    running_.store(false, std::memory_order_relaxed);
    if (thread_.joinable()) thread_.join();
}

void TestSource::Run() {
    const double period_s = static_cast<double>(frame_samples_) / 48000.0;
    const std::chrono::steady_clock::duration period =
        std::chrono::duration_cast<std::chrono::steady_clock::duration>(
            std::chrono::duration<double>(period_s));
    std::vector<int16_t> frame(frame_samples_);
    const double two_pi_f = 2.0 * M_PI * freq_hz_;
    uint64_t sample_index = 0;

    const auto start = std::chrono::steady_clock::now();
    auto next = start;
    while (running_.load(std::memory_order_relaxed)) {
        for (size_t i = 0; i < frame_samples_; ++i) {
            const double t = static_cast<double>(sample_index + i) / 48000.0;
            frame[i] = static_cast<int16_t>(amplitude_ * std::sin(two_pi_f * t));
        }
        sample_index += frame_samples_;
        Deliver(frame.data(), frame_samples_);
        // Absolute-schedule pacing: a missed deadline sleeps 0 and the next
        // frame re-syncs to the wall clock (drift correction).
        next += period;
        std::this_thread::sleep_until(next);
    }
}

}  // namespace openay
