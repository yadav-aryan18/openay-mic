# OpenAY Mic — Wire Protocol v1 (canonical spec)

This file is the single source of truth for the packet format shared by the
Android client (C++/Kotlin) and the desktop server (Rust). Any change here
must be reflected in both implementations and in `test-vectors.json`.

## Deviation from PLAN.md

PLAN.md specifies a 4-byte header with 1-byte fields. A 1-byte payload-length
field caps packets at 255 bytes, which cannot carry a 10 ms raw PCM frame at
48 kHz mono 16-bit (960 bytes) — one of the plan's own requirements. The
header is therefore **6 bytes** with 16-bit sequence and length fields.
Everything else follows the plan.

## Packet layout

All integers are **big-endian**. A packet is a 6-byte header followed by a
payload:

| Offset | Size | Field          | Notes                                        |
|--------|------|----------------|----------------------------------------------|
| 0      | 1    | magic          | `0xA7`                                       |
| 1      | 1    | type           | see Payload types                            |
| 2      | 2    | sequence       | `uint16`, per-direction counter              |
| 4      | 2    | payload length | `uint16`, bytes of payload that follow       |

Max packet size: 6 + 65535 bytes. Wi-Fi/UDP senders should keep payloads
<= 1400 bytes to stay inside a single MTU-safe datagram.

### Payload types

| Value | Name                  | Meaning                                              |
|-------|-----------------------|------------------------------------------------------|
| 0x00  | PCM                   | Raw PCM, signed 16-bit little-endian, mono, 48 kHz   |
| 0x01  | OPUS                  | One Opus packet, mono, 48 kHz                        |
| 0x02  | CONTROL               | UTF-8 JSON control message (handshake/stats/bye)     |
| other | reserved              | Receivers must count as malformed and drop           |

### Sequencing

- Each direction (client->server, server->client) keeps ONE counter shared by
  all packet types, incremented by 1 per packet, wrapping modulo 65536.
- Receiver-side classification against `expected = last_received + 1 (mod 2^16)`:
  - `seq == expected` -> in-order
  - `seq > expected` (mod-2^16 distance < 32768) -> gap of `(seq - expected)` lost packets
  - `seq == last` -> duplicate
  - otherwise -> out-of-order/reorder

### Audio framing

- PCM: frame sizes 5 ms (240 samples / 480 bytes) or 10 ms (480 samples /
  960 bytes); the length field disambiguates.
- Opus: exactly one Opus packet per audio frame; 10 ms frames (480 samples);
  encoder configured with `OPUS_APPLICATION_RESTRICTED_LOWDELAY`, 48 kHz,
  mono; default bitrate 32 kbps (configurable 16–96 kbps).

## Transport framing rules

- **UDP (Wi-Fi):** one datagram = one packet. Malformed datagrams are dropped
  and counted (`malformed`). Late packets are never retransmitted.
- **TCP (USB via `adb forward`):** packets are concatenated back-to-back on
  the stream; the receiver reads a 6-byte header, then exactly `payload_len`
  bytes. On a bad-magic byte the receiver scans forward up to 64 KiB for the
  next `0xA7`; if none is found the connection is a hard error.
- **Bluetooth RFCOMM:** byte-stream like TCP (same framing/resync rules).

## Interop test payload (deterministic filler)

Test/bench tools fill payloads with xorshift32 so both languages can verify
byte-exact content without sharing code:

```
state: uint32 = seq_number            # seed = packet sequence number
for each output byte:
    state ^= state << 13
    state ^= state >> 17
    state ^= state << 5               # all arithmetic mod 2^32
    emit (state & 0xFF)
```

Latency-bench payloads additionally start with an 8-byte **little-endian**
`uint64` monotonic-clock nanosecond timestamp (sender's clock), followed by
xorshift filler seeded with the sequence number. On loopback, sender and
receiver share `CLOCK_MONOTONIC`, so one-way delay is measurable across
processes and languages.

## Golden test vectors

See `test-vectors.json` (canonical). Both implementations must pass:
encode(expected fields) == hex bytes, and decode(hex bytes) == expected
fields / declared error.

Errors: `bad_magic` (first byte != 0xA7), `truncated` (fewer than 6 header
bytes, or fewer than `payload_len` payload bytes), `reserved_type`
(type not in {0,1,2}).
