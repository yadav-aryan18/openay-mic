// OpenAY Mic — Opus audio codec wrappers (48 kHz mono, 10 ms frames).
//
// The opus C state types (OpusEncoder/OpusDecoder from <opus.h>) are kept
// strictly inside src/opus_codec.cpp: this header stores opaque void*
// handles, so it compiles even on systems without libopus and never leaks
// opus type names into the openay namespace. Only src/opus_codec.cpp (built
// when OPENAY_HAVE_OPUS) includes <opus.h>.
#ifndef OPENAY_OPUS_CODEC_H
#define OPENAY_OPUS_CODEC_H

#include <cstdint>
#include <vector>

namespace openay {

// Encoder/decoder constants: 48 kHz mono, 10 ms frames (480 samples),
// OPUS_APPLICATION_RESTRICTED_LOWDELAY, default 32 kbps.
constexpr int kOpusSampleRate = 48000;
constexpr int kOpusChannels = 1;
constexpr int kOpusFrameSamples = 480;  // 10 ms at 48 kHz
constexpr int kOpusDefaultBitrate = 32000;

class OpusEncoder {
public:
    // Creates the encoder state and applies RESTRICTED_LOWDELAY + default
    // bitrate. On failure the encoder is invalid and the error is logged.
    OpusEncoder();
    ~OpusEncoder();
    OpusEncoder(const OpusEncoder&) = delete;
    OpusEncoder& operator=(const OpusEncoder&) = delete;

    bool Valid() const { return enc_ != nullptr; }

    // Bitrate in bits per second (project range 16000-96000).
    bool SetBitrate(int bps);

    // Encode one 480-sample mono frame. Returns false on invalid state or
    // encoder error (logged).
    bool Encode(const int16_t* pcm480, std::vector<uint8_t>* out);

private:
    void* enc_;  // ::OpusEncoder* (opaque outside opus_codec.cpp)
};

class OpusDecoder {
public:
    OpusDecoder();
    ~OpusDecoder();
    OpusDecoder(const OpusDecoder&) = delete;
    OpusDecoder& operator=(const OpusDecoder&) = delete;

    bool Valid() const { return dec_ != nullptr; }

    // Decode one Opus packet into a 480-sample mono frame. len == 0 (PLC
    // request) is rejected. Returns false on invalid state or decode error
    // (logged).
    bool Decode(const uint8_t* data, size_t len, int16_t* pcm480_out);

private:
    void* dec_;  // ::OpusDecoder* (opaque outside opus_codec.cpp)
};

}  // namespace openay

#endif  // OPENAY_OPUS_CODEC_H
