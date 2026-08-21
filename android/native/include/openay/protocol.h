// OpenAY Mic — wire protocol v1 (see shared/protocol.md, canonical).
//
// Packet layout (all integers big-endian):
//   [0]     magic          0xA7
//   [1]     type           0=PCM, 1=OPUS, 2=CONTROL (others reserved)
//   [2..3]  sequence       uint16, per-direction counter (mod 65536)
//   [4..5]  payload length uint16, bytes of payload that follow
//   [6..]   payload
//
// Golden test vectors live in shared/test-vectors.json (canonical);
// tests/test_protocol.cpp embeds the same bytes.
#ifndef OPENAY_PROTOCOL_H
#define OPENAY_PROTOCOL_H

#include <cstddef>
#include <cstdint>
#include <vector>

namespace openay {

constexpr uint8_t kMagic = 0xA7;
constexpr size_t kHeaderLen = 6;
// Maximum payload size addressable by the 16-bit length field.
constexpr size_t kMaxPayloadLen = 65535;

enum class PayloadType : uint8_t { Pcm = 0, Opus = 1, Control = 2 };

enum class DecodeError { BadMagic, Truncated, ReservedType };

struct Packet {
    PayloadType type = PayloadType::Pcm;
    uint16_t seq = 0;
    std::vector<uint8_t> payload;
};

// Internal big-endian load/store helpers (used by framing code and tools).
uint16_t LoadU16BE(const uint8_t* p);
void StoreU16BE(uint8_t* p, uint16_t v);

// Encode a packet into its wire form: 6-byte header + payload.
// Payloads larger than 65535 bytes are truncated to the wire maximum (with a
// logged warning) because the length field cannot express more.
std::vector<uint8_t> EncodePacket(const Packet& packet);

// Decode exactly one packet. The buffer must contain exactly
// 6 + payload_len bytes: fewer is DecodeError::Truncated, more is treated as
// trailing garbage (also a decode failure; Truncated is the closest declared
// error). Never throws; on failure *out is untouched.
bool DecodePacket(const uint8_t* data, size_t size, Packet* out, DecodeError* err);

// Validate a 6-byte stream header (magic + type) and return the payload
// length. Used by TCP/RFCOMM framing before reading the payload bytes.
bool HeaderPayloadLength(const uint8_t* header6, uint16_t* out, DecodeError* err);

}  // namespace openay

#endif  // OPENAY_PROTOCOL_H
