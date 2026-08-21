// Opus codec implementation — only built when OPENAY_HAVE_OPUS.
#include <opus.h>

#include <cstdio>
#include <cstdint>
#include <vector>

#include "openay/opus_codec.h"

namespace openay {
namespace {

// Worst-case Opus packet size (10 ms mono at 48 kHz stays far below this).
constexpr size_t kMaxOpusPacketBytes = 4000;

::OpusEncoder* AsEncoder(void* p) { return static_cast<::OpusEncoder*>(p); }
::OpusDecoder* AsDecoder(void* p) { return static_cast<::OpusDecoder*>(p); }

}  // namespace

// ---------------------------------------------------------------------------
// OpusEncoder
// ---------------------------------------------------------------------------

OpusEncoder::OpusEncoder() : enc_(nullptr) {
    int err = OPUS_OK;
    ::OpusEncoder* e =
        opus_encoder_create(kOpusSampleRate, kOpusChannels,
                            OPUS_APPLICATION_RESTRICTED_LOWDELAY, &err);
    if (err != OPUS_OK || !e) {
        fprintf(stderr, "openay: opus_encoder_create failed: %s\n",
                err != OPUS_OK ? opus_strerror(err) : "unknown");
        return;
    }
    if (opus_encoder_ctl(e, OPUS_SET_BITRATE(kOpusDefaultBitrate)) != OPUS_OK) {
        fprintf(stderr, "openay: OPUS_SET_BITRATE(%d) failed\n", kOpusDefaultBitrate);
    }
    enc_ = e;
}

OpusEncoder::~OpusEncoder() {
    if (enc_) opus_encoder_destroy(AsEncoder(enc_));
}

bool OpusEncoder::SetBitrate(int bps) {
    if (!enc_) {
        fprintf(stderr, "openay: SetBitrate on invalid encoder\n");
        return false;
    }
    if (opus_encoder_ctl(AsEncoder(enc_), OPUS_SET_BITRATE(bps)) != OPUS_OK) {
        fprintf(stderr, "openay: OPUS_SET_BITRATE(%d) failed\n", bps);
        return false;
    }
    return true;
}

bool OpusEncoder::Encode(const int16_t* pcm480, std::vector<uint8_t>* out) {
    if (!enc_ || !pcm480 || !out) {
        fprintf(stderr, "openay: OpusEncoder::Encode with invalid arguments\n");
        return false;
    }
    out->resize(kMaxOpusPacketBytes);
    const int rc = opus_encode(AsEncoder(enc_), pcm480, kOpusFrameSamples,
                               out->data(), static_cast<opus_int32>(out->size()));
    if (rc < 0) {
        fprintf(stderr, "openay: opus_encode failed: %s\n", opus_strerror(rc));
        return false;
    }
    out->resize(static_cast<size_t>(rc));
    return true;
}

// ---------------------------------------------------------------------------
// OpusDecoder
// ---------------------------------------------------------------------------

OpusDecoder::OpusDecoder() : dec_(nullptr) {
    int err = OPUS_OK;
    ::OpusDecoder* d = opus_decoder_create(kOpusSampleRate, kOpusChannels, &err);
    if (err != OPUS_OK || !d) {
        fprintf(stderr, "openay: opus_decoder_create failed: %s\n",
                err != OPUS_OK ? opus_strerror(err) : "unknown");
        return;
    }
    dec_ = d;
}

OpusDecoder::~OpusDecoder() {
    if (dec_) opus_decoder_destroy(AsDecoder(dec_));
}

bool OpusDecoder::Decode(const uint8_t* data, size_t len, int16_t* pcm480_out) {
    if (!dec_ || !data || len == 0 || !pcm480_out) {
        fprintf(stderr, "openay: OpusDecoder::Decode with invalid arguments\n");
        return false;
    }
    const int rc = opus_decode(AsDecoder(dec_), data, static_cast<opus_int32>(len),
                               pcm480_out, kOpusFrameSamples, 0);
    if (rc != kOpusFrameSamples) {
        fprintf(stderr, "openay: opus_decode returned %d (expected %d): %s\n", rc,
                kOpusFrameSamples,
                rc < 0 ? opus_strerror(rc) : "wrong frame size");
        return false;
    }
    return true;
}

}  // namespace openay
