// OpenAY Mic — protocol unit tests.
//
// Golden vectors below are byte-identical to shared/test-vectors.json, which
// is the canonical source of truth for the wire format.
#include "openay/protocol.h"

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

using openay::DecodeError;
using openay::DecodePacket;
using openay::EncodePacket;
using openay::HeaderPayloadLength;
using openay::kHeaderLen;
using openay::Packet;
using openay::PayloadType;

namespace {

int g_failures = 0;

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                \
        }                                                                \
    } while (0)

std::vector<uint8_t> HexBytes(const char* hex) {
    std::vector<uint8_t> v;
    const size_t n = strlen(hex);
    for (size_t i = 0; i + 1 < n; i += 2) {
        auto nib = [](char c) -> uint8_t {
            if (c >= '0' && c <= '9') return static_cast<uint8_t>(c - '0');
            return static_cast<uint8_t>(c - 'a' + 10);
        };
        v.push_back(static_cast<uint8_t>((nib(hex[i]) << 4) | nib(hex[i + 1])));
    }
    return v;
}

void TestGoldenVectors() {
    // Canonical: shared/test-vectors.json
    struct Case {
        const char* name;
        std::vector<uint8_t> bytes;
        PayloadType type;
        uint16_t seq;
        std::vector<uint8_t> payload;
        bool valid;
        DecodeError error;
    };
    const Case cases[] = {
        // pcm_basic
        {"pcm_basic", HexBytes("a70000010004deadbeef"), PayloadType::Pcm, 1,
         HexBytes("deadbeef"), true, DecodeError::BadMagic},
        // opus_high_seq
        {"opus_high_seq", HexBytes("a701fffe0003010203"), PayloadType::Opus, 65534,
         HexBytes("010203"), true, DecodeError::BadMagic},
        // control_hello
        {"control_hello", HexBytes("a7020000000568656c6c6f"), PayloadType::Control, 0,
         HexBytes("68656c6c6f"), true, DecodeError::BadMagic},
        // empty_payload
        {"empty_payload", HexBytes("a700ffff0000"), PayloadType::Pcm, 65535,
         {}, true, DecodeError::BadMagic},
        // bad_magic
        {"bad_magic", HexBytes("000000010004deadbeef"), PayloadType::Pcm, 0, {},
         false, DecodeError::BadMagic},
        // truncated_header
        {"truncated_header", HexBytes("a700000100"), PayloadType::Pcm, 0, {}, false,
         DecodeError::Truncated},
        // truncated_payload
        {"truncated_payload", HexBytes("a70000010004dead"), PayloadType::Pcm, 0, {},
         false, DecodeError::Truncated},
        // reserved_type
        {"reserved_type", HexBytes("a77f00010004deadbeef"), PayloadType::Pcm, 0, {},
         false, DecodeError::ReservedType},
    };

    for (const Case& c : cases) {
        Packet out;
        DecodeError err = DecodeError::BadMagic;
        const bool ok = DecodePacket(c.bytes.data(), c.bytes.size(), &out, &err);
        CHECK(ok == c.valid);
        if (c.valid) {
            CHECK(out.type == c.type);
            CHECK(out.seq == c.seq);
            CHECK(out.payload == c.payload);
            // Re-encoding must reproduce the golden bytes exactly.
            CHECK(EncodePacket(out) == c.bytes);
        } else {
            CHECK(err == c.error);
        }
    }

    // Zero-length input is truncated.
    {
        Packet out;
        DecodeError err = DecodeError::BadMagic;
        CHECK(!DecodePacket(nullptr, 0, &out, &err));
        CHECK(err == DecodeError::Truncated);
    }
}

void TestHeaderPayloadLength() {
    uint16_t len = 0;
    DecodeError err = DecodeError::BadMagic;
    // pcm_basic header: magic+type+seq 0001 + len 0004
    CHECK(HeaderPayloadLength(HexBytes("a70000010004").data(), &len, &err));
    CHECK(len == 4);
    // control_hello header -> len 5
    CHECK(HeaderPayloadLength(HexBytes("a70200000005").data(), &len, &err));
    CHECK(len == 5);
    // bad magic
    CHECK(!HeaderPayloadLength(HexBytes("000000010004").data(), &len, &err));
    CHECK(err == DecodeError::BadMagic);
    // reserved type
    CHECK(!HeaderPayloadLength(HexBytes("a77f00010004").data(), &len, &err));
    CHECK(err == DecodeError::ReservedType);
    // null out pointer is tolerated
    CHECK(HeaderPayloadLength(HexBytes("a70000010004").data(), nullptr, &err));
}

void TestEncodeDecodeRoundtrip() {
    const size_t sizes[] = {0, 1, 255, 256, 960, 1400};
    const PayloadType types[] = {PayloadType::Pcm, PayloadType::Opus,
                                 PayloadType::Control};
    for (size_t s : sizes) {
        for (size_t t = 0; t < 3; ++t) {
            Packet in;
            in.type = types[t];
            in.seq = static_cast<uint16_t>(0x1234 + s);
            in.payload.resize(s);
            for (size_t i = 0; i < s; ++i) {
                in.payload[i] = static_cast<uint8_t>((i * 7 + 3) & 0xFF);
            }
            const std::vector<uint8_t> wire = EncodePacket(in);
            CHECK(wire.size() == kHeaderLen + s);
            CHECK(wire[0] == openay::kMagic);

            Packet out;
            DecodeError err = DecodeError::BadMagic;
            CHECK(DecodePacket(wire.data(), wire.size(), &out, &err));
            CHECK(out.type == in.type);
            CHECK(out.seq == in.seq);
            CHECK(out.payload == in.payload);

            // A buffer with trailing garbage is not a well-formed packet.
            std::vector<uint8_t> extra = wire;
            extra.push_back(0x00);
            CHECK(!DecodePacket(extra.data(), extra.size(), &out, &err));
        }
    }
}

}  // namespace

int main() {
    TestGoldenVectors();
    TestHeaderPayloadLength();
    TestEncodeDecodeRoundtrip();
    if (g_failures == 0) {
        printf("test_protocol: all checks passed\n");
        return 0;
    }
    fprintf(stderr, "test_protocol: %d check(s) FAILED\n", g_failures);
    return 1;
}
