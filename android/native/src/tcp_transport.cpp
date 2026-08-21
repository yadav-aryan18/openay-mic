// TCP transport implementation: packets concatenated on a byte stream with
// 64 KiB bad-magic resync, per shared/protocol.md.
#include "openay/transport.h"

#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdio>
#include <vector>

#include "internal_log.h"

namespace openay {

namespace {

// Max bytes to scan for the next magic byte during stream resync.
constexpr size_t kMaxResyncScan = 65536;
// Max consecutive bad headers before we give up on a connection.
constexpr int kMaxBadHeaders = 4;

}  // namespace

// ---------------------------------------------------------------------------
// TcpConn
// ---------------------------------------------------------------------------

TcpConn::TcpConn(int fd) : fd_(fd) {
    // Disable Nagle: audio packets are small and must not be coalesced or
    // held back waiting for ACKs (latency requirement).
    if (fd_ >= 0) {
        const int one = 1;
        if (setsockopt(fd_, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one)) != 0) {
            detail::LogErrno("setsockopt(TCP_NODELAY)");
        }
    }
}

TcpConn::~TcpConn() {
    if (fd_ >= 0) close(fd_);
}

bool TcpConn::Send(const Packet& packet) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: TcpConn::Send on invalid connection\n");
        return false;
    }
    const std::vector<uint8_t> wire = EncodePacket(packet);
    size_t off = 0;
    while (off < wire.size()) {
        const ssize_t w = send(fd_, wire.data() + off, wire.size() - off, MSG_NOSIGNAL);
        if (w < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno("send");
            return false;
        }
        off += static_cast<size_t>(w);
    }
    return true;
}

bool TcpConn::ReadExact(uint8_t* dst, size_t n, int timeout_ms) {
    size_t off = 0;
    while (off < n) {
        pollfd pfd{fd_, POLLIN, 0};
        const int prc = poll(&pfd, 1, timeout_ms);
        if (prc < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno("poll");
            return false;
        }
        if (prc == 0) return false;  // timeout
        const ssize_t r = recv(fd_, dst + off, n - off, 0);
        if (r < 0) {
            if (errno == EINTR) continue;
            detail::LogErrno("recv");
            return false;
        }
        if (r == 0) {
            eof_ = true;  // clean EOF: peer closed
            return false;
        }
        off += static_cast<size_t>(r);
    }
    return true;
}

bool TcpConn::Resync(uint8_t* hdr_out, size_t* have_out, const uint8_t* already,
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
                    "openay: tcp resync failed: no 0xA7 within %zu bytes; "
                    "abandoning connection\n",
                    kMaxResyncScan);
            return false;
        }
    }
    uint8_t b = 0;
    while (scanned < kMaxResyncScan) {
        if (!ReadExact(&b, 1, timeout_ms)) {
            fprintf(stderr,
                    "openay: tcp resync aborted mid-scan (timeout/EOF) after "
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
            "openay: tcp resync failed: no 0xA7 within %zu bytes; abandoning "
            "connection\n",
            kMaxResyncScan);
    return false;
}

bool TcpConn::Recv(Packet* out, int timeout_ms) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: TcpConn::Recv on invalid connection\n");
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
                    "openay: tcp: %d consecutive invalid headers after resync; "
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

// ---------------------------------------------------------------------------
// TcpServer
// ---------------------------------------------------------------------------

TcpServer::TcpServer(uint16_t port) : port_(port) {}

TcpServer::~TcpServer() {
    if (fd_ >= 0) close(fd_);
}

bool TcpServer::Listen() {
    if (fd_ >= 0) {
        close(fd_);
        fd_ = -1;
    }
    fd_ = socket(AF_INET, SOCK_STREAM, 0);
    if (fd_ < 0) {
        detail::LogErrno("socket");
        return false;
    }
    const int one = 1;
    if (setsockopt(fd_, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one)) != 0) {
        detail::LogErrno("setsockopt(SO_REUSEADDR)");
    }
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    addr.sin_port = htons(port_);
    if (bind(fd_, reinterpret_cast<const sockaddr*>(&addr), sizeof(addr)) != 0) {
        detail::LogErrno("bind");
        close(fd_);
        fd_ = -1;
        return false;
    }
    if (listen(fd_, 8) != 0) {
        detail::LogErrno("listen");
        close(fd_);
        fd_ = -1;
        return false;
    }
    return true;
}

std::unique_ptr<TcpConn> TcpServer::Accept(int timeout_ms) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: TcpServer::Accept on non-listening server\n");
        return nullptr;
    }
    pollfd pfd{fd_, POLLIN, 0};
    const int prc = poll(&pfd, 1, timeout_ms);
    if (prc < 0) {
        if (errno == EINTR) return nullptr;
        detail::LogErrno("poll");
        return nullptr;
    }
    if (prc == 0) return nullptr;  // timeout
    const int cfd = accept(fd_, nullptr, nullptr);
    if (cfd < 0) {
        detail::LogErrno("accept");
        return nullptr;
    }
    return std::unique_ptr<TcpConn>(new TcpConn(cfd));
}

// ---------------------------------------------------------------------------
// TcpClient
// ---------------------------------------------------------------------------

TcpClient::TcpClient(const std::string& host, uint16_t port) : conn_(-1) {
    char portstr[16];
    snprintf(portstr, sizeof(portstr), "%u", static_cast<unsigned>(port));

    addrinfo hints{};
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* res = nullptr;
    const int rc = getaddrinfo(host.c_str(), portstr, &hints, &res);
    if (rc != 0) {
        fprintf(stderr, "openay: getaddrinfo(%s): %s\n", host.c_str(), gai_strerror(rc));
        return;
    }
    const int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) {
        detail::LogErrno("socket");
        freeaddrinfo(res);
        return;
    }
    if (connect(fd, res->ai_addr, res->ai_addrlen) != 0) {
        detail::LogErrno("connect");
        close(fd);
        freeaddrinfo(res);
        return;
    }
    freeaddrinfo(res);
    conn_ = TcpConn(fd);
}

TcpClient::~TcpClient() = default;

}  // namespace openay
