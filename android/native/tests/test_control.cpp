// OpenAY Mic — control vocabulary unit tests.
//
// The byte forms checked here are the exact serializations the Rust
// reference (openay_protocol::control) produces; both sides must stay
// byte-compatible per shared/protocol.md "Control messages".
#include "openay/control.h"

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

using openay::ControlKind;
using openay::EncodeControl;
using openay::ParseControl;

namespace {

int g_failures = 0;

#define CHECK(cond)                                                      \
    do {                                                                 \
        if (!(cond)) {                                                   \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
            ++g_failures;                                                \
        }                                                                \
    } while (0)

std::string AsText(const std::vector<uint8_t>& v) {
    return std::string(v.begin(), v.end());
}

}  // namespace

int main() {
    // --- serialization: exact wire forms -----------------------------------
    CHECK(AsText(EncodeControl(ControlKind::Ping, "", "", 0)) == "{\"t\":\"ping\"}");
    CHECK(AsText(EncodeControl(ControlKind::Pong, "", "", 0)) == "{\"t\":\"pong\"}");
    CHECK(AsText(EncodeControl(ControlKind::Bye, "", "", 0)) == "{\"t\":\"bye\"}");

    const auto hello = EncodeControl(ControlKind::Hello, "openay-mic",
                                     "pcm,opus", 48000);
    CHECK(AsText(hello) ==
          "{\"t\":\"hello\",\"app\":\"openay-mic\",\"proto\":1,"
          "\"codecs\":[\"pcm\",\"opus\"],\"rate\":48000}");

    const auto ack = EncodeControl(ControlKind::HelloAck, "openay-server",
                                   "pcm,opus", 48000);
    CHECK(AsText(ack).rfind("{\"t\":\"hello-ack\"", 0) == 0);

    // --- parsing: the Rust-produced forms ----------------------------------
    const auto parse = [](const std::string& json, std::string* app) {
        return ParseControl(reinterpret_cast<const uint8_t*>(json.data()),
                            json.size(), app);
    };

    std::string app;
    CHECK(parse(
        "{\"t\":\"hello\",\"app\":\"openay-mic\",\"proto\":1,"
        "\"codecs\":[\"pcm\",\"opus\"],\"rate\":48000}",
        &app) == ControlKind::Hello);
    CHECK(app == "openay-mic");

    app.clear();
    CHECK(parse(
        "{\"t\":\"hello-ack\",\"app\":\"openay-server\",\"proto\":1,"
        "\"codecs\":[\"pcm\",\"opus\"],\"rate\":48000}",
        &app) == ControlKind::HelloAck);
    CHECK(app == "openay-server");

    CHECK(parse("{\"t\":\"ping\"}", nullptr) == ControlKind::Ping);
    CHECK(parse("{\"t\":\"pong\"}", nullptr) == ControlKind::Pong);
    CHECK(parse("{\"t\":\"bye\"}", nullptr) == ControlKind::Bye);

    // hello without the optional fields still parses, without an app name.
    app.clear();
    CHECK(parse("{\"t\":\"hello\",\"app\":\"x\",\"proto\":1}", &app) ==
          ControlKind::Hello);
    CHECK(app == "x");

    // --- robustness: foreign/unknown/malformed input ------------------------
    CHECK(parse("{\"bye\":true}", nullptr) == ControlKind::Unknown);
    CHECK(parse("{\"t\":\"future-msg\"}", nullptr) == ControlKind::Unknown);
    CHECK(parse("not json", nullptr) == ControlKind::Unknown);
    CHECK(ParseControl(nullptr, 0, nullptr) == ControlKind::Unknown);
    // Unterminated string value.
    CHECK(parse("{\"t\":\"ping", nullptr) == ControlKind::Unknown);

    // Oversized payloads are rejected without scanning.
    std::vector<uint8_t> big(2048, 'x');
    CHECK(ParseControl(big.data(), big.size(), nullptr) == ControlKind::Unknown);

    // --- hostile inputs: escaped quotes cannot forge a tag ------------------
    // A JSON string VALUE containing an escaped quote cannot smuggle a fake
    // "t" tag: the scanner works on raw bytes, and JSON escapes inner quotes
    // as backslash-quote, which never forms the three-byte "t" sequence.
    CHECK(parse(
        "{\"app\":\"x\\\"t\\\":\\\"ping\\\"\",\"t\":\"hello\"}",
        &app) == ControlKind::Hello);
    // The scanner stops at the first quote byte (it does not unescape — our
    // vocabulary's values never contain quotes); the important property is
    // that the escaped text could not forge the kind.
    CHECK(app == "x\\");
    // Keys whose names merely contain 't' do not match the tag scanner.
    CHECK(parse("{\"not_t\":\"x\",\"t\":\"ping\"}", nullptr) == ControlKind::Ping);
    // Unknown fields are ignored (same default as the Rust serde parser).
    CHECK(parse("{\"t\":\"pong\",\"extra\":123}", nullptr) == ControlKind::Pong);
    CHECK(parse("{\"t\":\"ping\",\"app\":\"whoever\"}", nullptr) == ControlKind::Ping);

    // Round-trip: every serialized kind parses back to the same kind.
    for (ControlKind k : {ControlKind::Hello, ControlKind::HelloAck,
                          ControlKind::Ping, ControlKind::Pong,
                          ControlKind::Bye}) {
        const auto wire = EncodeControl(k, "openay-mic", "pcm,opus", 48000);
        CHECK(ParseControl(wire.data(), wire.size(), nullptr) == k);
    }

    if (g_failures != 0) {
        fprintf(stderr, "test_control: %d failure(s)\n", g_failures);
        return 1;
    }
    printf("test_control: all checks passed\n");
    return 0;
}
