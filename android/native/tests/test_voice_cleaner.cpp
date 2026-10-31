// OpenAY Mic — voice FX (voice_cleaner) unit tests.
//
// The cleaner is our own capture-side cleanup stage: a floor-limited gate
// (downward expander) followed by a slow target-RMS AGC with a peak
// limiter. These tests pin the behavioral contract that matters for the
// reported audio issues:
//
//   1. The gate attenuates background noise, but never below its floor
//      (-14 dB): suppression is bounded, never a mute.
//   2. Word tails survive: a decaying syllable trailing into the noise
//      floor is attenuated far less than the platform's hard gate would,
//      and the release is slower than the attack.
//   3. The AGC pulls a quiet capture up toward the target RMS (the desk
//      scenario) and pulls an over-loud capture down, without pumping
//      within a sustained tone.
//   4. The limiter keeps every output sample inside full scale even with
//      maximum AGC gain on a loud input.
//   5. Disabled mode is a pass-through.
#include "openay/voice_cleaner.h"

#include <atomic>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <vector>

using openay::VoiceCleaner;

namespace {

std::atomic<int> g_failures{0};

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            std::printf("CHECK failed at %s:%d: %s\n", __FILE__,         \
                        __LINE__, #cond);                                \
            g_failures.fetch_add(1);                                     \
        }                                                                \
    } while (0)

constexpr size_t kFrame = 480;  // 10 ms @ 48 kHz
constexpr double kPi = 3.14159265358979323846;

float FrameRms(const std::vector<int16_t>& s) {
    double acc = 0.0;
    for (int16_t v : s) {
        const double x = static_cast<double>(v) / 32768.0;
        acc += x * x;
    }
    return static_cast<float>(std::sqrt(acc / static_cast<double>(s.size())));
}

std::vector<int16_t> Tone(float amplitude, float freq_hz, size_t n) {
    std::vector<int16_t> out(n);
    for (size_t i = 0; i < n; ++i) {
        out[i] = static_cast<int16_t>(
            amplitude * 32767.0f *
            std::sin(2.0f * static_cast<float>(kPi) * freq_hz *
                     static_cast<float>(i) / 48000.0f));
    }
    return out;
}

/// Steady low-level noise (small random-ish values via an LCG).
std::vector<int16_t> Noise(float amplitude, size_t n, uint32_t seed) {
    std::vector<int16_t> out(n);
    uint32_t st = seed ? seed : 1u;
    for (size_t i = 0; i < n; ++i) {
        st = st * 1664525u + 1013904223u;
        const float u =
            static_cast<float>(static_cast<int32_t>(st >> 8) % 20001) /
                10000.0f -
            1.0f;
        out[i] = static_cast<int16_t>(amplitude * u * 32767.0f);
    }
    return out;
}

/// Drives the cleaner with `frames` *fresh copies* of `frame` (the capture
/// pipeline always delivers newly captured samples — re-processing the same
/// buffer in place would feed back the output as input), and returns the RMS
/// of the last emitted frame (steady state).
float SteadyRms(VoiceCleaner& c, const std::vector<int16_t>& frame,
                int frames) {
    std::vector<int16_t> work;
    for (int f = 0; f < frames; ++f) {
        work = frame;
        c.Process(work.data(), work.size());
    }
    return FrameRms(work);
}

/// 1. Gate bounds: steady noise is ducked but never near-muted, and the
/// attenuation stays at or above the floor-limited minimum.
void TestGateIsBounded() {
    VoiceCleaner c;
    c.Reset(kFrame);
    c.set_enabled(true);
    // Noise at ~-40 dBFS RMS with a few frames of lead-in for floor setup.
    const auto noise = Noise(0.003f, kFrame, 7u);
    const float in_rms = FrameRms(noise);
    const float out_rms = SteadyRms(c, noise, 300);
    CHECK(out_rms < in_rms * 0.9f);   // ducked at all
    CHECK(out_rms > in_rms * 0.1f);   // never muted (floor-limited)
}

/// 2. Word tails survive: a tone decaying into the noise floor keeps most
/// of its energy relative to the hard-cut case (fast attack / slow release).
void TestTailSurvival() {
    VoiceCleaner c;
    c.Reset(kFrame);
    c.set_enabled(true);
    // Establish the floor with quiet noise, then a sustained tone (gate
    // opens, gain settles), then a tail frame at 1/8 of the tone's
    // amplitude. The cleaner's release must keep the tail well above what
    // an instantaneous gate would leave.
    const auto noise = Noise(0.003f, kFrame, 11u);
    std::vector<int16_t> work;
    for (int f = 0; f < 200; ++f) {
        work = noise;
        c.Process(work.data(), work.size());
    }
    const auto tone = Tone(0.2f, 440.0f, kFrame);
    for (int f = 0; f < 60; ++f) {
        work = tone;
        c.Process(work.data(), work.size());
    }
    // Tail: same waveform at 1/8 amplitude — above the noise floor, but
    // fading toward it.
    std::vector<int16_t> tail = tone;
    for (int16_t& v : tail) v = static_cast<int16_t>(v / 8);
    const float tail_in = FrameRms(tail);
    c.Process(tail.data(), tail.size());
    const float tail_out = FrameRms(tail);
    // An instantaneous gate would leave ~0 here; the bounded expander must
    // keep at least half of the tail's level.
    CHECK(tail_out > tail_in * 0.5f);
}

/// 3. AGC pulls quiet captures toward the target RMS and loud ones down.
/// The input models speech (tone bursts separated by low-level gaps — a
/// constant tone is indistinguishable from noise to minimum-statistics
/// floor tracking, by design).
void TestAgcNormalizesLevel() {
    const auto gap = Noise(0.003f, kFrame, 5u);
    const auto measure = [&](float amplitude) {
        VoiceCleaner c;
        c.Reset(kFrame);
        c.set_enabled(true);
        const auto burst = Tone(amplitude, 440.0f, kFrame);
        std::vector<int16_t> work;
        float last = 0.0f;
        for (int f = 0; f < 600; ++f) {
            for (int b = 0; b < 3; ++b) {
                work = burst;
                c.Process(work.data(), work.size());
                last = FrameRms(work);
            }
            for (int g = 0; g < 7; ++g) {
                work = gap;
                c.Process(work.data(), work.size());
            }
        }
        return last;
    };

    // Desk-distance capture: tone at ~-33 dBFS peak — the AGC must pull it
    // up toward the -20 dBFS target (the "phone on the desk" complaint).
    const float quiet_in = FrameRms(Tone(0.03f, 440.0f, kFrame));
    const float quiet_out = measure(0.03f);
    CHECK(quiet_out > quiet_in * 2.0f);
    CHECK(quiet_out < 0.25f);  // converges near the target, bounded

    // Close talk: loud tone — the AGC must pull it DOWN, not clip.
    const float loud_in = FrameRms(Tone(0.7f, 440.0f, kFrame));
    const float loud_out = measure(0.7f);
    CHECK(loud_out < loud_in);
    CHECK(loud_out > 0.02f);  // not squashed to silence
}

/// 4. Limiter: the loudest possible input never leaves the output above
/// full scale, even after the AGC has raised its gain.
void TestLimiterKeepsFullScale() {
    VoiceCleaner c;
    c.Reset(kFrame);
    c.set_enabled(true);
    const auto hot = Tone(0.98f, 440.0f, kFrame);
    std::vector<int16_t> work = hot;
    for (int f = 0; f < 600; ++f) {
        c.Process(work.data(), work.size());
        for (int16_t v : work) {
            CHECK(v <= 32767 && v >= -32767);
        }
    }
}

/// 5. Disabled cleaner is a pass-through (bit-exact).
void TestDisabledIsPassthrough() {
    VoiceCleaner c;
    c.Reset(kFrame);
    c.set_enabled(false);
    auto work = Tone(0.123f, 440.0f, kFrame);
    const auto original = work;
    c.Process(work.data(), work.size());
    CHECK(work == original);
}

}  // namespace

int main() {
    TestGateIsBounded();
    TestTailSurvival();
    TestAgcNormalizesLevel();
    TestLimiterKeepsFullScale();
    TestDisabledIsPassthrough();
    if (g_failures.load() != 0) {
        std::printf("FAILED: %d check(s)\n", g_failures.load());
        return 1;
    }
    std::printf("voice_cleaner: all checks passed\n");
    return 0;
}
