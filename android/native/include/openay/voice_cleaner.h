// OpenAY Mic — per-frame speech cleanup (voice FX), original implementation.
//
// Two stages applied in place to each captured frame (before encoding):
//
// 1. **Gate (downward expander)** — attenuates steady background noise
//    between words. The noise floor is tracked per frame with minimum
//    statistics (instantaneous drop to any quieter frame, slow rise
//    otherwise), and the attenuation is *floor-limited*: the gate never
//    mutes, it ducks at most ~14 dB, with a fast attack and a slow release
//    so word tails and breaths pass through instead of being chopped.
//
// 2. **AGC** — normalizes speech loudness toward a target RMS so the
//    receiving side is audible at unity gain regardless of how far the
//    phone sits from the speaker. The gain moves slowly (no pumping),
//    freezes while the gate is closed (so it never chases the noise floor
//    upward), and a per-frame peak limiter with instant attack / slow
//    release keeps the boosted signal inside full scale without hard
//    clipping.
//
// All time constants are per-frame fractions tuned for 10 ms frames (the
// 5 ms frame size sees proportionally faster adaptation — acceptable for a
// capture-side cleanup). Real-time safe: no allocation, no locks, O(n) in
// the frame length.
#ifndef OPENAY_VOICE_CLEANER_H_
#define OPENAY_VOICE_CLEANER_H_

#include <algorithm>

namespace openay {

class VoiceCleaner {
public:
    // Frame geometry (samples per frame) — resets all state.
    void Reset(size_t frame_samples) {
        frame_samples_ = frame_samples > 0 ? frame_samples : 1;
        noise_floor_ = kFloorInit;
        gate_gain_ = 1.0f;
        gate_open_ = false;
        quiet_run_ = 0;
        agc_gain_ = 1.0f;
        limiter_ = 1.0f;
    }

    // Enables/disables processing without clearing adaptation state
    // (toggling the FX mid-run resumes where it left off).
    void set_enabled(bool on) { enabled_ = on; }
    bool enabled() const { return enabled_; }

    float agc_gain() const { return agc_gain_; }

    // Processes one frame in place (native int16 samples).
    void Process(int16_t* samples, size_t n);

private:
    // --- gate (noise floor + bounded expander) ------------------------------
    // Initial noise-floor estimate, linear (~ -50 dBFS): low enough that the
    // gate starts fully open in anything but digital silence.
    static constexpr float kFloorInit = 0.003f;
    // Floor rise per frame when the current frame is louder than the floor
    // (1.001 at 100 fps ≈ +10 %/s — far slower than speech pauses, so the
    // floor tracks the *background*, not the speech).
    static constexpr float kFloorRise = 1.001f;
    // Absolute floor bound: below this the gate threshold would never open
    // on very quiet devices, so clamp the estimate.
    static constexpr float kFloorMin = 0.0002f;
    // Gate hysteresis, as factors of the tracked noise floor. Opens at
    // +9.5 dB over the floor (speech is typically 10-30x), closes below
    // +6 dB. Breaths land between the two, so they pass through.
    static constexpr float kGateOpenFactor = 3.0f;
    static constexpr float kGateCloseFactor = 2.0f;
    // Quiet frames tolerated at unity before the gate starts ducking
    // (~80 ms at 10 ms frames): short inter-word pauses and syllable gaps
    // must not be ducked at all, or each new word starts swallowed.
    static constexpr int kGateHoldFrames = 8;
    // Floor-limited attenuation when the gate is closed: ~-14 dB, never a
    // mute — the difference between "cleanup" and "chopped words".
    static constexpr float kGateMinGain = 0.2f;
    // Gate smoothing per frame: reopening is fast (~3 frames to 90 %, click
    // safe), closing is gradual over ~10 frames after the hold.
    static constexpr float kGateOpenSpeed = 0.5f;
    static constexpr float kGateCloseSpeed = 0.2f;

    // --- AGC ----------------------------------------------------------------
    // Speech target, linear RMS (~ -20 dBFS): peaks land ~3-4x that, inside
    // the limiter's headroom.
    static constexpr float kTargetRms = 0.1f;
    // Frames quieter than this are not speech — the AGC holds its gain.
    static constexpr float kAgcMinRms = 0.005f;
    // Per-frame multiplicative step bounds (at most ±6 dB per frame): a
    // 20 dB correction takes ~5 frames (~50 ms) — fast enough to catch the
    // start of a sentence, slow enough to never pump within a word. The
    // gain itself converges to the full target/rms correction.
    static constexpr float kAgcMaxStep = 2.0f;
    static constexpr float kAgcMinStep = 0.5f;
    // Gain adaptation speeds: moderately fast attack, slow release.
    static constexpr float kAgcAttack = 0.15f;
    static constexpr float kAgcRelease = 0.03f;
    // Gain ceiling: +21.5 dB — a desk-distance phone (mic signal around
    // -35 dBFS) reaches the target without entering the limiter; closer
    // speech simply needs less.
    static constexpr float kAgcGainMax = 12.0f;
    static constexpr float kAgcGainMin = 0.25f;

    // --- limiter --------------------------------------------------------------
    // Peak ceiling per frame (~-0.4 dBFS) and the slow release toward unity.
    static constexpr float kLimitCeil = 0.95f;
    static constexpr float kLimitRelease = 0.02f;

    size_t frame_samples_ = 1;
    bool enabled_ = false;

    float noise_floor_ = kFloorInit;
    float gate_gain_ = 1.0f;
    bool gate_open_ = false;
    int quiet_run_ = 0;
    float agc_gain_ = 1.0f;
    float limiter_ = 1.0f;
};

}  // namespace openay

#endif  // OPENAY_VOICE_CLEANER_H_
