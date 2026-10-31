// fd transport implementation: BytePipe over an owned file descriptor — the
// native side of the Bluetooth RFCOMM (SPP) bridge (see transport.h).
//
// The wire bytes are byte-for-byte what TcpConn::Send/Recv put on a TCP
// stream (EncodePacket's 6-byte header + payload, written in full; the same
// header validation, 64 KiB bad-magic resync, and partial-read retention on
// the receive side), so the desktop and phone stream decoders are shared
// between the TCP and Bluetooth transports.
#include "openay/transport.h"

#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <vector>

#include "internal_log.h"

namespace openay {

namespace {
// Same limits as the TCP stream decoder (tcp_transport.cpp).
constexpr size_t kMaxResyncScan = 65536;
constexpr int kMaxBadHeaders = 4;
}  // namespace

FdBytePipe::FdBytePipe(int fd) : fd_(fd) {
    // A stalled RFCOMM link (RF degradation, desktop paused) must become a
    // reportable send error instead of a blocked network thread: cap each
    // write at ~2 s. The socketpair peer sees the same timeout on its reads.
    if (fd_ >= 0) {
        timeval tv{};
        tv.tv_sec = 2;
        tv.tv_usec = 0;
        if (setsockopt(fd_, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv)) != 0) {
            detail::LogErrno("setsockopt(SO_SNDTIMEO)");
        }
    }
}

FdBytePipe::~FdBytePipe() {
    if (fd_ >= 0) close(fd_);  // exactly once: the fd is owned (see transport.h)
}

bool FdBytePipe::Push(const uint8_t* data, size_t size) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: FdBytePipe::Push on invalid fd\n");
        return false;
    }
    // Full-write loop, mirroring TcpConn::Send. send(MSG_NOSIGNAL) is tried
    // first so socket fds never raise SIGPIPE; the production fd is a pipe
    // (Kotlin's ParcelFileDescriptor.createPipe), which reports ENOTSOCK and
    // flips the pipe to write() permanently. write() on Android is SIGPIPE
    // safe because ART blocks the signal process-wide (EPIPE comes back).
    size_t off = 0;
    while (off < size) {
        ssize_t w;
        if (socket_) {
            w = send(fd_, data + off, size - off, MSG_NOSIGNAL);
            if (w < 0 && errno == ENOTSOCK) {
                socket_ = false;
                continue;
            }
        } else {
            w = write(fd_, data + off, size - off);
        }
        if (w < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno(socket_ ? "send" : "write");
            return false;
        }
        off += static_cast<size_t>(w);
    }
    return true;
}

ssize_t FdBytePipe::Pull(uint8_t* data, size_t max_size) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: FdBytePipe::Pull on invalid fd\n");
        return -1;
    }
    for (;;) {
        const ssize_t r = read(fd_, data, max_size);
        if (r < 0 && errno == EINTR) continue;
        return r;  // > 0 bytes, 0 on clean EOF, -1 on error
    }
}

// ---------------------------------------------------------------------------
// Framed receive (desktop->phone control traffic): the same rules as
// TcpConn::Recv — 6-byte header, payload-length validation, 64 KiB
// bad-magic resync, <= 4 consecutive bad headers, and bytes consumed by an
// aborted read retained in `pend_` across calls.
// ---------------------------------------------------------------------------

bool FdBytePipe::ReadExact(uint8_t* dst, size_t n, int timeout_ms) {
    size_t off = 0;
    // Serve bytes retained by an aborted previous read first (a stream may
    // split a packet across the poll-timeout boundary).
    if (!pend_.empty()) {
        const size_t take = std::min(n, pend_.size());
        std::memcpy(dst, pend_.data(), take);
        pend_.erase(pend_.begin(), pend_.begin() + static_cast<std::ptrdiff_t>(take));
        off = take;
        if (off == n) return true;
    }
    while (off < n) {
        pollfd pfd{fd_, POLLIN, 0};
        const int prc = poll(&pfd, 1, timeout_ms);
        if (prc < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno("poll");
            RetainPartial(dst, off);
            return false;
        }
        if (prc == 0) {  // timeout
            RetainPartial(dst, off);
            return false;
        }
        const ssize_t r = read(fd_, dst + off, n - off);
        if (r < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno("read");
            // A permanent receive error (ECONNRESET/EPIPE) is the link being
            // dead, same as EOF: make it sticky so the ~1 ms control drain
            // stops hammering a broken fd with poll+read+log.
            if (errno == ECONNRESET || errno == EPIPE) eof_ = true;
            RetainPartial(dst, off);
            return false;
        }
        if (r == 0) {
            eof_ = true;  // clean EOF: peer closed; nothing to retain
            return false;
        }
        off += static_cast<size_t>(r);
    }
    return true;
}

void FdBytePipe::RetainPartial(const uint8_t* data, size_t n) {
    if (n > 0) pend_.insert(pend_.end(), data, data + n);
}

bool FdBytePipe::Resync(uint8_t* hdr_out, size_t* have_out, const uint8_t* already,
                        size_t already_n, int timeout_ms) {
    size_t scanned = 0;
    // Bytes we already consumed from the stream but never validated may still
    // contain the next header's magic byte; if so, the header continues with
    // the buffered bytes after it.
    for (size_t i = 0; i < already_n; ++i) {
        if (already[i] == kMagic) {
            const size_t have = already_n - i - 1;
            hdr_out[0] = kMagic;
            if (have > 0) std::memcpy(hdr_out + 1, already + i + 1, have);
            *have_out = have;
            return true;
        }
        if (++scanned >= kMaxResyncScan) {
            fprintf(stderr,
                    "openay: fd resync failed: no 0xA7 within %zu bytes; "
                    "abandoning connection\n",
                    kMaxResyncScan);
            return false;
        }
    }
    uint8_t b = 0;
    while (scanned < kMaxResyncScan) {
        if (!ReadExact(&b, 1, timeout_ms)) {
            fprintf(stderr,
                    "openay: fd resync aborted mid-scan (timeout/EOF) after "
                    "%zu bytes\n",
                    scanned);
            return false;
        }
        ++scanned;
        if (b == kMagic) {
            hdr_out[0] = kMagic;
            *have_out = 0;
            return true;
        }
    }
    fprintf(stderr,
            "openay: fd resync failed: no 0xA7 within %zu bytes; abandoning "
            "connection\n",
            kMaxResyncScan);
    return false;
}

bool FdBytePipe::Recv(Packet* out, int timeout_ms) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: FdBytePipe::Recv on invalid fd\n");
        return false;
    }
    if (eof_) return false;

    uint8_t hdr[6];
    if (!ReadExact(hdr, sizeof(hdr), timeout_ms)) return false;

    // Validate the header; on failure resync to the next 0xA7 and retry.
    uint16_t plen = 0;
    int bad_headers = 0;
    while (true) {
        DecodeError err = DecodeError::BadMagic;
        if (HeaderPayloadLength(hdr, &plen, &err)) break;
        stats_.malformed++;
        if (++bad_headers > kMaxBadHeaders) {
            fprintf(stderr,
                    "openay: fd: %d consecutive invalid headers after resync; "
                    "abandoning connection\n",
                    bad_headers);
            return false;
        }
        size_t have = 0;
        if (!Resync(hdr, &have, hdr + 1, sizeof(hdr) - 1, timeout_ms)) return false;
        if (have < sizeof(hdr) - 1 &&
            !ReadExact(hdr + 1 + have, sizeof(hdr) - 1 - have, timeout_ms)) {
            return false;
        }
    }

    std::vector<uint8_t> payload(plen);
    if (plen > 0 && !ReadExact(payload.data(), plen, timeout_ms)) return false;

    out->type = static_cast<PayloadType>(hdr[1]);
    out->seq = LoadU16BE(hdr + 2);
    out->payload = std::move(payload);
    NotePacket(stats_, seq_, out->seq);
    return true;
}

}  // namespace openay
