// OpenAY Mic — receiver statistics and stream sequencing (header-only).
#ifndef OPENAY_STATS_H
#define OPENAY_STATS_H

#include <cstdint>
#include <string>

namespace openay {

struct PacketStats {
    uint64_t received = 0;        // well-formed packets decoded
    uint64_t lost = 0;            // packets inferred missing (gap distance)
    uint64_t duplicate = 0;       // seq == last received seq
    uint64_t out_of_order = 0;    // backward jump / reorder
    uint64_t malformed = 0;       // undecodable datagrams / bad stream bytes
    uint64_t content_errors = 0;  // payload-level verification failures
};

// Exact rendering contract for tools and tests; do not change.
inline std::string FormatStats(const PacketStats& s) {
    return "RECV ok=" + std::to_string(s.received) +
           " lost=" + std::to_string(s.lost) +
           " dup=" + std::to_string(s.duplicate) +
           " ooo=" + std::to_string(s.out_of_order) +
           " malformed=" + std::to_string(s.malformed) +
           " content_errors=" + std::to_string(s.content_errors);
}

enum class SeqEvent { InOrder, Gap, Duplicate, Reorder };

// Mod-2^16 sequence classifier, identical semantics to shared/protocol.md:
//   seq == expected (last + 1 mod 2^16)            -> InOrder
//   forward distance (seq - expected) mod 2^16 < 32768 -> Gap, count = distance
//   seq == last                                    -> Duplicate
//   otherwise                                      -> Reorder
// Only InOrder/Gap advance the stream position; Duplicate/Reorder do not.
class SeqTracker {
public:
    SeqEvent Update(uint16_t seq, uint16_t* gap_count) {
        if (!have_last_) {
            have_last_ = true;
            last_ = seq;
            return SeqEvent::InOrder;
        }
        const uint16_t expected = static_cast<uint16_t>(last_ + 1);
        if (seq == expected) {
            last_ = seq;
            return SeqEvent::InOrder;
        }
        if (seq == last_) {
            return SeqEvent::Duplicate;
        }
        const uint16_t forward = static_cast<uint16_t>(seq - expected);
        if (forward < 32768) {
            if (gap_count) *gap_count = forward;
            last_ = seq;
            return SeqEvent::Gap;
        }
        return SeqEvent::Reorder;
    }

private:
    bool have_last_ = false;
    uint16_t last_ = 0;
};

// Record a received packet into `stats` using a stream tracker: bumps
// received and classifies the seq (lost += gap distance, duplicate++,
// out_of_order++ as appropriate). Shared by the UDP and TCP receivers.
inline void NotePacket(PacketStats& stats, SeqTracker& tracker, uint16_t seq) {
    stats.received++;
    uint16_t gap = 0;
    switch (tracker.Update(seq, &gap)) {
        case SeqEvent::InOrder:
            break;
        case SeqEvent::Gap:
            stats.lost += gap;
            break;
        case SeqEvent::Duplicate:
            stats.duplicate++;
            break;
        case SeqEvent::Reorder:
            stats.out_of_order++;
            break;
    }
}

}  // namespace openay

#endif  // OPENAY_STATS_H
