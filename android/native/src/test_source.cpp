// Real-time sine generator implementation (see test_source.h).
#include "openay/test_source.h"

#include <chrono>
#include <cmath>
#include <vector>

namespace openay {

TestSource::TestSource(double freq_hz, double amplitude, size_t frame_samples,
                       size_t onset_frames)
    : freq_hz_(freq_hz), amplitude_(amplitude), frame_samples_(frame_samples),
      onset_frames_(onset_frames) {}

bool TestSource::Start() {
    if (running_.load(std::memory_order_relaxed)) return true;  // idempotent
    // Per-run accounting: a Start->Stop->Start cycle must not accumulate the
    // previous run's frame counts (tone-udp reads silence_frames_delivered()
    // after Stop to report onset_packet).
    silence_frames_.store(0, std::memory_order_relaxed);
    sine_frames_.store(0, std::memory_order_relaxed);
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
    std::vector<int16_t> frame(frame_samples_);  // value-init: all zeros
    const double two_pi_f = 2.0 * M_PI * freq_hz_;
    uint64_t sample_index = 0;
    uint64_t silence_left = onset_frames_;

    const auto start = std::chrono::steady_clock::now();
    auto next = start;
    while (running_.load(std::memory_order_relaxed)) {
        if (silence_left > 0) {
            // Onset gate: leading digital silence. The frame vector is
            // value-initialized to all zeros, so the block is sample-exact
            // silence from frame 0 with no per-sample fill.
            --silence_left;
            silence_frames_.fetch_add(1, std::memory_order_relaxed);
            Deliver(frame.data(), frame_samples_);
        } else {
            for (size_t i = 0; i < frame_samples_; ++i) {
                const double t = static_cast<double>(sample_index + i) / 48000.0;
                frame[i] = static_cast<int16_t>(amplitude_ * std::sin(two_pi_f * t));
            }
            sample_index += frame_samples_;
            sine_frames_.fetch_add(1, std::memory_order_relaxed);
            Deliver(frame.data(), frame_samples_);
        }
        // Absolute-schedule pacing: a missed deadline sleeps 0 and the next
        // frame re-syncs to the wall clock (drift correction). The silence
        // frames pace at the same period, so the onset lands exactly
        // onset_frames_ * frame_period after Start(), and the sine resumes at
        // phase 0 (sample_index only advances on sine frames).
        next += period;
        std::this_thread::sleep_until(next);
    }
}

}  // namespace openay
