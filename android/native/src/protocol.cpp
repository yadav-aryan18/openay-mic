#include "openay/protocol.h"

#include <cstdio>
#include <cstring>

namespace openay {

uint16_t LoadU16BE(const uint8_t* p) {
    return static_cast<uint16_t>((static_cast<uint16_t>(p[0]) << 8) | p[1]);
}

void StoreU16BE(uint8_t* p, uint16_t v) {
    p[0] = static_cast<uint8_t>(v >> 8);
    p[1] = static_cast<uint8_t>(v & 0xFF);
}

std::vector<uint8_t> EncodePacket(const Packet& packet) {
    size_t plen = packet.payload.size();
    if (plen > kMaxPayloadLen) {
        fprintf(stderr,
                "openay: EncodePacket: payload of %zu bytes exceeds the 16-bit "
                "length field; truncating to %zu\n",
                plen, kMaxPayloadLen);
        plen = kMaxPayloadLen;
    }
    std::vector<uint8_t> out(kHeaderLen + plen);
    out[0] = kMagic;
    out[1] = static_cast<uint8_t>(packet.type);
    StoreU16BE(&out[2], packet.seq);
    StoreU16BE(&out[4], static_cast<uint16_t>(plen));
    if (plen > 0) {
        std::memcpy(&out[kHeaderLen], packet.payload.data(), plen);
    }
    return out;
}

bool DecodePacket(const uint8_t* data, size_t size, Packet* out, DecodeError* err) {
    // Fewer than 6 bytes cannot even carry a header.
    if (size < kHeaderLen) {
        if (err) *err = DecodeError::Truncated;
        return false;
    }
    if (data[0] != kMagic) {
        if (err) *err = DecodeError::BadMagic;
        return false;
    }
    const uint8_t t = data[1];
    if (t > static_cast<uint8_t>(PayloadType::Control)) {
        if (err) *err = DecodeError::ReservedType;
        return false;
    }
    const uint16_t plen = LoadU16BE(data + 4);
    // Spec: a packet is exactly 6 + payload_len bytes. Fewer -> truncated;
    // more means trailing garbage (e.g. an oversized datagram) and is likewise
    // not a well-formed packet.
    if (size != kHeaderLen + plen) {
        if (err) *err = DecodeError::Truncated;
        return false;
    }
    out->type = static_cast<PayloadType>(t);
    out->seq = LoadU16BE(data + 2);
    out->payload.assign(data + kHeaderLen, data + kHeaderLen + plen);
    return true;
}

bool HeaderPayloadLength(const uint8_t* header6, uint16_t* out, DecodeError* err) {
    if (header6[0] != kMagic) {
        if (err) *err = DecodeError::BadMagic;
        return false;
    }
    const uint8_t t = header6[1];
    if (t > static_cast<uint8_t>(PayloadType::Control)) {
        if (err) *err = DecodeError::ReservedType;
        return false;
    }
    if (out) *out = LoadU16BE(header6 + 4);
    return true;
}

}  // namespace openay
