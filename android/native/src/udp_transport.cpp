// UDP transport implementation: one datagram per packet.
#include "openay/transport.h"

#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdio>
#include <vector>

#include "internal_log.h"

namespace openay {

// ---------------------------------------------------------------------------
// UdpSender
// ---------------------------------------------------------------------------

UdpSender::UdpSender(const std::string& host, uint16_t port) {
    char portstr[16];
    snprintf(portstr, sizeof(portstr), "%u", static_cast<unsigned>(port));

    addrinfo hints{};
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_DGRAM;
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
    // connect() pins the peer so Send can use send(); no bytes are sent.
    if (connect(fd, res->ai_addr, res->ai_addrlen) != 0) {
        detail::LogErrno("connect");
        close(fd);
        freeaddrinfo(res);
        return;
    }
    freeaddrinfo(res);
    fd_ = fd;
}

UdpSender::~UdpSender() {
    if (fd_ >= 0) close(fd_);
}

bool UdpSender::Send(const Packet& packet) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: UdpSender::Send on invalid sender\n");
        return false;
    }
    const std::vector<uint8_t> wire = EncodePacket(packet);
    const ssize_t n = send(fd_, wire.data(), wire.size(), 0);
    if (n < 0) {
        detail::LogErrno("send");
        return false;
    }
    if (static_cast<size_t>(n) != wire.size()) {
        fprintf(stderr, "openay: UdpSender::Send short datagram: %zd of %zu bytes\n", n,
                wire.size());
        return false;
    }
    return true;
}

// ---------------------------------------------------------------------------
// UdpReceiver
// ---------------------------------------------------------------------------

UdpReceiver::UdpReceiver(uint16_t port) : port_(port) {}

UdpReceiver::~UdpReceiver() {
    if (fd_ >= 0) close(fd_);
}

bool UdpReceiver::Bind() {
    if (fd_ >= 0) {
        close(fd_);
        fd_ = -1;
    }
    fd_ = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd_ < 0) {
        detail::LogErrno("socket");
        return false;
    }
    const int one = 1;
    if (setsockopt(fd_, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one)) != 0) {
        detail::LogErrno("setsockopt(SO_REUSEADDR)");
    }
    // Absorb sender bursts (jitter headroom); kernel caps at rmem_max and
    // truncates silently otherwise. Matches the desktop receiver's sizing.
    int rcvbuf = 4 * 1024 * 1024;
    if (setsockopt(fd_, SOL_SOCKET, SO_RCVBUF, &rcvbuf, sizeof(rcvbuf)) != 0) {
        detail::LogErrno("setsockopt(SO_RCVBUF)");
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
    return true;
}

bool UdpReceiver::Recv(Packet* out, int timeout_ms) {
    if (fd_ < 0) {
        fprintf(stderr, "openay: UdpReceiver::Recv on unbound receiver\n");
        return false;
    }
    pollfd pfd{fd_, POLLIN, 0};
    const int prc = poll(&pfd, 1, timeout_ms);
    if (prc < 0) {
        if (errno == EINTR) return false;
        detail::LogErrno("poll");
        return false;
    }
    if (prc == 0) return false;  // timeout

    // Max IPv4 UDP payload is 65507; 65541 leaves room for the header.
    uint8_t buf[65541];
    const ssize_t n = recv(fd_, buf, sizeof(buf), 0);
    if (n < 0) {
        if (errno == EINTR) return false;
        detail::LogErrno("recvfrom");
        return false;
    }
    DecodeError err = DecodeError::Truncated;
    if (!DecodePacket(buf, static_cast<size_t>(n), out, &err)) {
        stats_.malformed++;  // dropped, never fatal
        return false;
    }
    NotePacket(stats_, seq_, out->seq);
    return true;
}

}  // namespace openay
