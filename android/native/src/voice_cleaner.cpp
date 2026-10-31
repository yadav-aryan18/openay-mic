// Voice FX per-frame processing (see voice_cleaner.h for the design and the
// tuning constants). All state is scalar; one frame costs a handful of O(n)
// passes over 480 samples — negligible next to the codec.
#include "openay/voice_cleaner.h"

#include <cmath>

namespace openay {

void VoiceCleaner::Process(int16_t* samples, size_t n) {
    if (!enabled_ || n == 0) return;

    // Frame RMS of the raw capture, linear (int16 full scale = 1.0).
    double acc = 0.0;
    for (size_t i = 0; i < n; ++i) {
        const float s = static_cast<float>(samples[i]) / 32768.0f;
        acc += static_cast<double>(s) * s;
    }
    const float rms = static_cast<float>(std::sqrt(acc / static_cast<double>(n)));

    // --- noise floor: minimum statistics with a slow rise -------------------
    // Any quieter frame pulls the floor down instantly; louder frames let it
    // creep up slowly, so it converges on the *background*, not the speech.
    if (rms < noise_floor_) {
        noise_floor_ = rms;
    } else {
        noise_floor_ *= kFloorRise;
    }
    if (noise_floor_ < kFloorMin) noise_floor_ = kFloorMin;

    // --- gate: hysteresis against the floor, hold, bounded attenuation ------
    if (gate_open_) {
        if (rms < kGateCloseFactor * noise_floor_) {
            gate_open_ = false;
            quiet_run_ = 0;
        }
    } else if (rms > kGateOpenFactor * noise_floor_) {
        gate_open_ = true;
        quiet_run_ = 0;
    }
    // Unity during the hold (brief pauses stay untouched), duck only after
    // sustained quiet. Reopening is fast so word onsets are not swallowed.
    float gate_target = 1.0f;
    if (!gate_open_) {
        if (quiet_run_ < kGateHoldFrames) {
            ++quiet_run_;
        } else {
            gate_target = kGateMinGain;
        }
    }
    const float gate_alpha =
        gate_target > gate_gain_ ? kGateOpenSpeed : kGateCloseSpeed;
    gate_gain_ += gate_alpha * (gate_target - gate_gain_);

    // --- AGC: slow target-RMS normalization, frozen while gated -------------
    if (gate_open_ && rms > kAgcMinRms) {
        // Full correction toward the target (bounded by the absolute gain
        // range), approached through per-frame multiplicative steps so a
        // 20 dB correction takes ~5 frames instead of one jump.
        const float desired =
            std::min(kAgcGainMax, std::max(kAgcGainMin, kTargetRms / rms));
        const float stepped = std::min(
            desired,
            std::max(desired, agc_gain_ * kAgcMaxStep));
        const float stepped_down = std::max(
            agc_gain_ * kAgcMinStep, stepped);
        const float agc_alpha =
            stepped_down > agc_gain_ ? kAgcAttack : kAgcRelease;
        agc_gain_ += agc_alpha * (stepped_down - agc_gain_);
        agc_gain_ =
            std::min(kAgcGainMax, std::max(kAgcGainMin, agc_gain_));
    }

    // --- apply both gains, then limit ---------------------------------------
    const float total = gate_gain_ * agc_gain_;
    float peak = 0.0f;
    for (size_t i = 0; i < n; ++i) {
        const float s = static_cast<float>(samples[i]) / 32768.0f * total;
        samples[i] = static_cast<int16_t>(
            std::max(-32767.0f, std::min(32767.0f, s * 32768.0f)));
        const float a = std::fabs(static_cast<float>(samples[i])) / 32768.0f;
        if (a > peak) peak = a;
    }
    // Instant attack (a peak over the ceiling must be caught this frame),
    // slow release back to unity (no post-sentence dip).
    if (peak > kLimitCeil) {
        limiter_ = kLimitCeil / peak;
    } else {
        limiter_ += (1.0f - limiter_) * kLimitRelease;
    }
    if (limiter_ < 0.999f) {
        for (size_t i = 0; i < n; ++i) {
            samples[i] = static_cast<int16_t>(static_cast<float>(samples[i]) *
                                              limiter_);
        }
    }
}

}  // namespace openay
