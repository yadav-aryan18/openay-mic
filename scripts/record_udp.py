#!/usr/bin/env python3
"""OpenAY Mic UDP capture receiver.

Listens for OpenAY wire packets (shared/protocol.md), validates framing and
sequence continuity, and writes any PCM payload audio to a WAV file.

Exit code 0 when the capture window elapsed and at least one valid packet
arrived; 1 when nothing valid was received.
"""

from __future__ import annotations

import argparse
import array
import socket
import struct
import sys
import time
import wave
from pathlib import Path

MAGIC: int = 0xA7
TYPE_PCM: int = 0
TYPE_OPUS: int = 1
SAMPLE_RATE: int = 48_000


class SeqStats:
    """Tracks sequence continuity across received packets."""

    def __init__(self) -> None:
        self.last_seq: int | None = None
        self.lost = 0
        self.duplicate = 0
        self.out_of_order = 0

    def update(self, seq: int) -> None:
        if self.last_seq is None:
            self.last_seq = seq
            return
        expected = (self.last_seq + 1) & 0xFFFF
        if seq == self.last_seq:
            self.duplicate += 1
        elif seq == expected:
            pass
        else:
            forward = (seq - expected) & 0xFFFF
            if forward < 0x8000:
                self.lost += forward
            else:
                self.out_of_order += 1
        self.last_seq = seq


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--seconds", type=float, default=10.0)
    parser.add_argument("--out", type=Path, default=Path("capture.wav"))
    parser.add_argument("--bind", default="0.0.0.0")
    parser.add_argument(
        "--tcp",
        action="store_true",
        help="accept one TCP connection instead of listening on UDP",
    )
    return parser.parse_args()


def recv_stream(
    sock: socket.socket,
    seconds: float,
    pcm_samples: array.array[int],
    stats: SeqStats,
) -> tuple[int, int, int]:
    """Receive OpenAY packets until `seconds` elapse.

    Returns (packets, pcm_packets, malformed). Works for both a UDP socket
    (one datagram per packet) and an accepted TCP stream (self-framing via
    the header length field).
    """
    packets = pcm_packets = malformed = 0
    buf = bytearray()
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            if sock.type == socket.SOCK_DGRAM:
                data, _ = sock.recvfrom(65_541)
            else:
                data = sock.recv(65_541)
        except socket.timeout:
            continue
        except OSError:
            break
        if not data:
            break
        buf += data
        while True:
            if len(buf) < 6:
                break
            if buf[0] != MAGIC:
                # Resync: scan forward for the next magic byte (TCP rule).
                buf = buf[1:]
                malformed += 1
                continue
            (plen,) = struct.unpack(">H", buf[4:6])
            if len(buf) < 6 + plen:
                break
            ptype = buf[1]
            (seq,) = struct.unpack(">H", buf[2:4])
            payload = bytes(buf[6 : 6 + plen])
            del buf[: 6 + plen]
            if ptype not in (TYPE_PCM, TYPE_OPUS):
                malformed += 1
                continue
            stats.update(seq)
            packets += 1
            if ptype == TYPE_PCM:
                pcm_packets += 1
                pcm_samples.frombytes(payload)
            else:
                pass
    return packets, pcm_packets, malformed


def main() -> int:
    args = parse_args()

    pcm_samples: array.array[int] = array.array("h")
    stats = SeqStats()

    if args.tcp:
        server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        server.bind((args.bind, args.port))
        server.listen(1)
        server.settimeout(args.seconds + 5.0)
        conn, addr = server.accept()
        print(f"connection from {addr}", file=sys.stderr)
        conn.settimeout(2.0)
        try:
            packets, pcm_packets, malformed = recv_stream(
                conn, args.seconds, pcm_samples, stats
            )
        finally:
            conn.close()
            server.close()
        opus_packets = 0
    else:
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024 * 1024)
        sock.bind((args.bind, args.port))
        sock.settimeout(2.0)
        packets, pcm_packets, malformed = recv_stream(
            sock, args.seconds, pcm_samples, stats
        )
        sock.close()
        opus_packets = packets - pcm_packets

    duration = len(pcm_samples) / SAMPLE_RATE
    rms = (
        (sum(x * x for x in pcm_samples) / len(pcm_samples)) ** 0.5
        if pcm_samples
        else 0.0
    )

    wrote_wav = False
    if pcm_packets > 0:
        with wave.open(str(args.out), "wb") as wav:
            wav.setnchannels(1)
            wav.setsampwidth(2)
            wav.setframerate(SAMPLE_RATE)
            wav.writeframes(pcm_samples.tobytes())
        wrote_wav = True

    print(
        f"RECORD packets={packets} pcm={pcm_packets} opus={opus_packets} "
        f"lost={stats.lost} dup={stats.duplicate} ooo={stats.out_of_order} "
        f"malformed={malformed} duration_s={duration:.2f} rms={rms:.1f} "
        f"wav={'yes' if wrote_wav else 'no'}"
    )
    return 0 if packets > 0 else 1


if __name__ == "__main__":
    sys.exit(main())
