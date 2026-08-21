// OpenAY Mic — host-only test/sim audio source: real-time sine generator.
//
// Generates a mono 48 kHz sine at a configurable frequency and amplitude
// (default 440 Hz, 0.4 * 32767) and invokes the registered callback with
// `frame_samples`-sample blocks (default 480 = 10 ms) paced in real time via
// std::this_thread::sleep_until.
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
    // frame_samples: samples per delivered block (480 = 10 ms @ 48 kHz).
    explicit TestSource(double freq_hz = 440.0, double amplitude = 0.4 * 32767.0,
                        size_t frame_samples = 480);
    ~TestSource() override { Stop(); }

    bool Start() override;  // spawns the generator thread
    void Stop() override;   // joins it
    // sharing_mode()/xruns()/error_string() keep the IAudioSource defaults
    // ("exclusive" / 0 / "").

private:
    void Run();

    const double freq_hz_;
    const double amplitude_;
    const size_t frame_samples_;
    std::thread thread_;
    std::atomic<bool> running_{false};
};

}  // namespace openay

#endif  // OPENAY_TEST_SOURCE_H
