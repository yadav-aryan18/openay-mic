// OpenAY Mic — host-only test/sim audio source: real-time sine generator.
//
// Generates a mono 48 kHz sine at a configurable frequency and amplitude
// (default 440 Hz, 0.4 * 32767) and invokes the registered callback with
// `frame_samples`-sample blocks (default 480 = 10 ms) paced in real time via
// std::this_thread::sleep_until.
//
// Onset gate: an optional `onset_frames` count of leading digital-silence
// frames (sample-exact all-zero blocks, paced at the same period) precedes
// the sine; the sine starts at phase 0 the moment the onset hits. tone-udp's
// --onset-after uses this to place a deterministic tone boundary mid-stream
// for software latency measurement (the silence frames pace at the same
// period, so the onset lands exactly onset_frames * frame_period after
// Start()).
//
// Drift correction: pacing uses an absolute schedule (deadline += period), so
// a delivery that misses its deadline (sleep_until already in the past) does
// not sleep and the generator re-syncs to the wall clock instead of
// accumulating lag.
//
// Host-only: used by the host test suite and the tone-udp loopback tool; on
// Android the engine uses OboeSource instead. The code itself is portable
// (chrono/thread/cmath) so it also compiles cleanly in the NDK build.
#ifndef OPENAY_TEST_SOURCE_H
#define OPENAY_TEST_SOURCE_H

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <thread>

#include "openay/audio_source.h"

namespace openay {

class TestSource : public IAudioSource {
public:
    // freq_hz: sine frequency; amplitude: peak sample value;
    // frame_samples: samples per delivered block (480 = 10 ms @ 48 kHz);
    // onset_frames: leading digital-silence frames delivered before the sine
    // (0 = sine from frame 0; the sine always starts at phase 0 when the
    // onset hits, regardless of onset_frames).
    explicit TestSource(double freq_hz = 440.0, double amplitude = 0.4 * 32767.0,
                        size_t frame_samples = 480, size_t onset_frames = 0);
    ~TestSource() override { Stop(); }

    bool Start() override;  // spawns the generator thread
    void Stop() override;   // joins it
    // sharing_mode()/xruns()/error_string() keep the IAudioSource defaults
    // ("exclusive" / 0 / "").

    // Frame accounting (atomic, safe from any thread): how many leading
    // silence / sine frames have been delivered so far. tone-udp reads these
    // after Stop() (which joins the generator thread) to derive the onset
    // packet index: with exactly one 480-sample frame per packet, the
    // 0-based index of the first non-silent packet equals the number of
    // silence frames actually handed over.
    uint64_t silence_frames_delivered() const {
        return silence_frames_.load(std::memory_order_relaxed);
    }
    uint64_t sine_frames_delivered() const {
        return sine_frames_.load(std::memory_order_relaxed);
    }

private:
    void Run();

    const double freq_hz_;
    const double amplitude_;
    const size_t frame_samples_;
    const size_t onset_frames_;
    std::thread thread_;
    std::atomic<bool> running_{false};
    std::atomic<uint64_t> silence_frames_{0};
    std::atomic<uint64_t> sine_frames_{0};
};

}  // namespace openay

#endif  // OPENAY_TEST_SOURCE_H
