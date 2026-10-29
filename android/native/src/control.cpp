// Control vocabulary implementation (see control.h).
#include "openay/control.h"

#include <cstring>

namespace openay {

namespace {

constexpr size_t kMaxControlPayload = 1024;

// Finds `"key"` followed by `:` and a quoted string value; copies the value
// into `out`. Returns false when the key or its quoted value is absent.
// The scanner is field-order independent and never reads past `size`.
bool ExtractString(const uint8_t* data, size_t size, const char* key,
                   std::string* out) {
    const size_t key_len = std::strlen(key);
    if (size < key_len + 4) return false;
    for (size_t i = 0; i + key_len + 4 <= size; ++i) {
        if (std::memcmp(data + i, key, key_len) != 0) continue;
        size_t j = i + key_len;
        // Skip whitespace and exactly one colon.
        while (j < size && (data[j] == ' ' || data[j] == ':')) ++j;
        if (j >= size || data[j] != '"') continue;
        ++j;
        size_t end = j;
        while (end < size && data[end] != '"') ++end;
        if (end >= size) continue;  // unterminated
        out->assign(reinterpret_cast<const char*>(data + j), end - j);
        return true;
    }
    return false;
}

}  // namespace

ControlKind ParseControl(const uint8_t* data, size_t size, std::string* app) {
    if (!data || size == 0 || size > kMaxControlPayload) {
        return ControlKind::Unknown;
    }
    std::string tag;
    if (!ExtractString(data, size, "\"t\"", &tag)) {
        return ControlKind::Unknown;
    }
    ControlKind kind = ControlKind::Unknown;
    if (tag == "hello") kind = ControlKind::Hello;
    else if (tag == "hello-ack") kind = ControlKind::HelloAck;
    else if (tag == "ping") kind = ControlKind::Ping;
    else if (tag == "pong") kind = ControlKind::Pong;
    else if (tag == "bye") kind = ControlKind::Bye;
    if (kind == ControlKind::Unknown) return kind;

    if (app && (kind == ControlKind::Hello || kind == ControlKind::HelloAck)) {
        ExtractString(data, size, "\"app\"", app);
    }
    return kind;
}

std::vector<uint8_t> EncodeControl(ControlKind kind, const std::string& app,
                                   const std::string& codecs_csv,
                                   uint32_t rate) {
    std::string s = "{\"t\":\"";
    switch (kind) {
        case ControlKind::Hello: s += "hello"; break;
        case ControlKind::HelloAck: s += "hello-ack"; break;
        case ControlKind::Ping: s += "ping"; break;
        case ControlKind::Pong: s += "pong"; break;
        case ControlKind::Bye: s += "bye"; break;
        case ControlKind::Unknown: s += "unknown"; break;
    }
    s += "\"";
    if (kind == ControlKind::Hello || kind == ControlKind::HelloAck) {
        s += ",\"app\":\"" + app + "\"";
        s += ",\"proto\":" + std::to_string(kControlProto);
        // Codecs as a JSON array from the comma-separated form.
        s += ",\"codecs\":[";
        std::string item;
        bool first = true;
        for (size_t i = 0; i <= codecs_csv.size(); ++i) {
            if (i == codecs_csv.size() || codecs_csv[i] == ',') {
                if (!item.empty()) {
                    if (!first) s += ",";
                    s += "\"" + item + "\"";
                    first = false;
                    item.clear();
                }
            } else if (codecs_csv[i] != ' ') {
                item += codecs_csv[i];
            }
        }
        s += "]";
        s += ",\"rate\":" + std::to_string(rate);
    }
    s += "}";
    return std::vector<uint8_t>(s.begin(), s.end());
}

}  // namespace openay
