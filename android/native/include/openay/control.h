// OpenAY Mic — control-plane JSON vocabulary for the CONTROL payload type
// (additive v1 extension; see shared/protocol.md "Control messages").
//
// Fixed vocabulary, hand-serialized: the core has no JSON dependency. The
// byte forms here must stay compatible with openay_protocol::control (Rust):
//   {"t":"hello","app":"openay-mic","proto":1,"codecs":["pcm","opus"],"rate":48000}
//   {"t":"hello-ack",...same shape...}
//   {"t":"ping"}  {"t":"pong"}  {"t":"bye"}
//
// Parsing is deliberately narrow: only these exact `t` tags are recognized,
// anything else (foreign JSON, unknown tags, garbage, oversize) is Unknown —
// a control message is never fatal to the stream.
#ifndef OPENAY_CONTROL_H
#define OPENAY_CONTROL_H

#include <cstddef>
#include <cstdint>
#include <string>
#include <vector>

namespace openay {

enum class ControlKind : int {
    Unknown = 0,
    Hello = 1,
    HelloAck = 2,
    Ping = 3,
    Pong = 4,
    Bye = 5,
};

inline const char* ControlKindName(ControlKind k) {
    switch (k) {
        case ControlKind::Hello: return "hello";
        case ControlKind::HelloAck: return "hello-ack";
        case ControlKind::Ping: return "ping";
        case ControlKind::Pong: return "pong";
        case ControlKind::Bye: return "bye";
        case ControlKind::Unknown: break;
    }
    return "unknown";
}

// Control-vocabulary version announced in hello/hello-ack (protocol.md).
constexpr uint8_t kControlProto = 1;

// Parses a CONTROL payload. `app` (optional) receives the peer's app
// identifier when the message carries one. Returns Unknown for anything not
// in the vocabulary; payloads over 1024 bytes are rejected outright.
ControlKind ParseControl(const uint8_t* data, size_t size, std::string* app);

// Serializes a control message. `app` / `codecs_csv` / `rate` are only used
// for hello/hello-ack (codecs as comma-separated names, e.g. "pcm,opus";
// pass "" / 0 for the others).
std::vector<uint8_t> EncodeControl(ControlKind kind, const std::string& app,
                                   const std::string& codecs_csv,
                                   uint32_t rate);

}  // namespace openay

#endif  // OPENAY_CONTROL_H
